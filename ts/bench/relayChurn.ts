import {
  finalizeEvent,
  type Filter,
  generateSecretKey,
  getPublicKey,
  matchFilter,
  type UnsignedEvent,
  type VerifiedEvent,
} from "nostr-tools";

import { NdrRuntime } from "../src/NdrRuntime";
import { InMemoryStorageAdapter } from "../src/StorageAdapter";
import {
  type NostrPublish,
  type NostrSubscribe,
  type Rumor,
} from "../src/types";

const BENCH_NAME = "ndr-relay-churn";

type ClientName = "alice" | "bob" | "carol";
type Scenario = "direct" | "group";

interface BenchPayload {
  bench: typeof BENCH_NAME;
  runId: string;
  scenario: Scenario;
  phase: "warmup" | "measure";
  id: string;
  from: ClientName;
  to?: ClientName;
  sentAt: number;
  index: number;
}

interface ClientRelayStats {
  subscribeCalls: number;
  unsubscribeCalls: number;
  publishCalls: number;
  replayedEvents: number;
  liveEventsDelivered: number;
  deliveredEvents: number;
  subscriptionBytes: number;
  publishBytes: number;
  deliveredBytes: number;
}

interface RelayStats {
  subscribeCalls: number;
  unsubscribeCalls: number;
  publishCalls: number;
  replayedEvents: number;
  liveEventsDelivered: number;
  deliveredEvents: number;
  subscriptionBytes: number;
  publishBytes: number;
  deliveredBytes: number;
  storedEvents: number;
  activeSubscriptions: number;
  maxActiveSubscriptions: number;
  byClient: Record<ClientName, ClientRelayStats>;
}

interface Subscription {
  id: string;
  client: ClientName;
  filter: Filter;
  onEvent: (event: VerifiedEvent) => void;
}

interface Participant {
  name: ClientName;
  ownerPubkey: string;
  runtime: NdrRuntime;
}

interface LatencySummary {
  count: number;
  minMs: number;
  p50Ms: number;
  p95Ms: number;
  maxMs: number;
  avgMs: number;
}

interface ScenarioMetrics {
  sent: number;
  expectedDeliveries: number;
  receivedDeliveries: number;
  timedOutDeliveries: number;
  duplicateDecodedDeliveries: number;
  unexpectedDeliveries: number;
  latency: LatencySummary;
}

function makeClientStats(): ClientRelayStats {
  return {
    subscribeCalls: 0,
    unsubscribeCalls: 0,
    publishCalls: 0,
    replayedEvents: 0,
    liveEventsDelivered: 0,
    deliveredEvents: 0,
    subscriptionBytes: 0,
    publishBytes: 0,
    deliveredBytes: 0,
  };
}

function makeRelayStats(): RelayStats {
  return {
    subscribeCalls: 0,
    unsubscribeCalls: 0,
    publishCalls: 0,
    replayedEvents: 0,
    liveEventsDelivered: 0,
    deliveredEvents: 0,
    subscriptionBytes: 0,
    publishBytes: 0,
    deliveredBytes: 0,
    storedEvents: 0,
    activeSubscriptions: 0,
    maxActiveSubscriptions: 0,
    byClient: {
      alice: makeClientStats(),
      bob: makeClientStats(),
      carol: makeClientStats(),
    },
  };
}

function isReplaceableKind(kind: number): boolean {
  return (
    kind === 0 ||
    kind === 3 ||
    (kind >= 10_000 && kind < 20_000) ||
    (kind >= 30_000 && kind < 40_000)
  );
}

function replaceableKey(event: VerifiedEvent): string {
  if (event.kind >= 30_000 && event.kind < 40_000) {
    const dTag = event.tags.find((tag) => tag[0] === "d")?.[1] || "";
    return `${event.kind}:${event.pubkey}:${dTag}`;
  }
  return `${event.kind}:${event.pubkey}`;
}

function dedupeReplaceable(events: VerifiedEvent[]): VerifiedEvent[] {
  const latestReplaceable = new Map<string, VerifiedEvent>();
  const nonReplaceable: VerifiedEvent[] = [];

  for (const event of events) {
    if (!isReplaceableKind(event.kind)) {
      nonReplaceable.push(event);
      continue;
    }

    const key = replaceableKey(event);
    const existing = latestReplaceable.get(key);
    if (!existing || event.created_at >= existing.created_at) {
      latestReplaceable.set(key, event);
    }
  }

  return [...nonReplaceable, ...latestReplaceable.values()];
}

function wireBytes(value: unknown): number {
  return Buffer.byteLength(JSON.stringify(value));
}

class InstrumentedRelay {
  private events: VerifiedEvent[] = [];
  private subscriptions = new Map<string, Subscription>();
  private stats = makeRelayStats();
  private nextSubId = 0;

  subscribe(
    client: ClientName,
    filter: Filter,
    onEvent: (event: VerifiedEvent) => void,
  ): { id: string; close: () => void } {
    const id = `${client}-sub-${++this.nextSubId}`;
    const subscription: Subscription = { id, client, filter, onEvent };
    this.subscriptions.set(id, subscription);

    const subscriptionBytes = wireBytes(["REQ", id, filter]);
    this.stats.subscribeCalls += 1;
    this.stats.subscriptionBytes += subscriptionBytes;
    this.stats.activeSubscriptions = this.subscriptions.size;
    this.stats.maxActiveSubscriptions = Math.max(
      this.stats.maxActiveSubscriptions,
      this.stats.activeSubscriptions,
    );
    const clientStats = this.stats.byClient[client];
    clientStats.subscribeCalls += 1;
    clientStats.subscriptionBytes += subscriptionBytes;

    const matches = dedupeReplaceable(
      this.events.filter((event) => matchFilter(filter, event)),
    );
    for (const event of matches) {
      this.deliver(subscription, event, "replay");
    }

    let closed = false;
    return {
      id,
      close: () => {
        if (closed) return;
        closed = true;
        if (!this.subscriptions.delete(id)) return;
        this.stats.unsubscribeCalls += 1;
        this.stats.activeSubscriptions = this.subscriptions.size;
        this.stats.byClient[client].unsubscribeCalls += 1;
      },
    };
  }

  storeAndDeliver(client: ClientName, event: VerifiedEvent): void {
    this.events.push(event);
    this.stats.storedEvents = this.events.length;
    this.stats.publishCalls += 1;
    this.stats.publishBytes += wireBytes(["EVENT", event]);
    const clientStats = this.stats.byClient[client];
    clientStats.publishCalls += 1;
    clientStats.publishBytes += wireBytes(["EVENT", event]);

    for (const subscription of Array.from(this.subscriptions.values())) {
      if (matchFilter(subscription.filter, event)) {
        this.deliver(subscription, event, "live");
      }
    }
  }

  resetCounters(): void {
    const activeSubscriptions = this.subscriptions.size;
    this.stats = makeRelayStats();
    this.stats.storedEvents = this.events.length;
    this.stats.activeSubscriptions = activeSubscriptions;
    this.stats.maxActiveSubscriptions = activeSubscriptions;
  }

  snapshot(): RelayStats {
    return JSON.parse(JSON.stringify(this.stats)) as RelayStats;
  }

  private deliver(
    subscription: Subscription,
    event: VerifiedEvent,
    source: "live" | "replay",
  ): void {
    const bytes = wireBytes(["EVENT", subscription.id, event]);
    this.stats.deliveredEvents += 1;
    this.stats.deliveredBytes += bytes;
    const clientStats = this.stats.byClient[subscription.client];
    clientStats.deliveredEvents += 1;
    clientStats.deliveredBytes += bytes;

    if (source === "replay") {
      this.stats.replayedEvents += 1;
      clientStats.replayedEvents += 1;
    } else {
      this.stats.liveEventsDelivered += 1;
      clientStats.liveEventsDelivered += 1;
    }

    subscription.onEvent(event);
  }
}

class DeliveryMetrics {
  sent = 0;
  expectedDeliveries = 0;
  receivedDeliveries = 0;
  timedOutDeliveries = 0;
  duplicateDecodedDeliveries = 0;
  unexpectedDeliveries = 0;
  readonly latencies: number[] = [];
  private readonly seen = new Set<string>();

  recordSend(expectedDeliveries: number): void {
    this.sent += 1;
    this.expectedDeliveries += expectedDeliveries;
  }

  recordDelivery(deliveryKey: string, latencyMs: number): void {
    if (this.seen.has(deliveryKey)) {
      this.duplicateDecodedDeliveries += 1;
      return;
    }
    this.seen.add(deliveryKey);
    this.receivedDeliveries += 1;
    this.latencies.push(latencyMs);
  }

  recordTimeout(missingDeliveries: number): void {
    this.timedOutDeliveries += missingDeliveries;
  }

  recordUnexpected(): void {
    this.unexpectedDeliveries += 1;
  }

  summary(): ScenarioMetrics {
    return {
      sent: this.sent,
      expectedDeliveries: this.expectedDeliveries,
      receivedDeliveries: this.receivedDeliveries,
      timedOutDeliveries: this.timedOutDeliveries,
      duplicateDecodedDeliveries: this.duplicateDecodedDeliveries,
      unexpectedDeliveries: this.unexpectedDeliveries,
      latency: summarizeLatency(this.latencies),
    };
  }
}

function summarizeLatency(values: number[]): LatencySummary {
  if (values.length === 0) {
    return {
      count: 0,
      minMs: 0,
      p50Ms: 0,
      p95Ms: 0,
      maxMs: 0,
      avgMs: 0,
    };
  }

  const sorted = [...values].sort((a, b) => a - b);
  const percentile = (fraction: number) => {
    const index = Math.min(
      sorted.length - 1,
      Math.max(0, Math.ceil(sorted.length * fraction) - 1),
    );
    return sorted[index]!;
  };
  const total = sorted.reduce((sum, value) => sum + value, 0);
  return {
    count: sorted.length,
    minMs: sorted[0]!,
    p50Ms: percentile(0.5),
    p95Ms: percentile(0.95),
    maxMs: sorted[sorted.length - 1]!,
    avgMs: Math.round((total / sorted.length) * 10) / 10,
  };
}

function parsePayload(content: string): BenchPayload | null {
  try {
    const value = JSON.parse(content) as Partial<BenchPayload>;
    if (value.bench !== BENCH_NAME) return null;
    if (typeof value.runId !== "string") return null;
    if (value.scenario !== "direct" && value.scenario !== "group") return null;
    if (value.phase !== "warmup" && value.phase !== "measure") return null;
    if (typeof value.id !== "string") return null;
    if (!isClientName(value.from)) return null;
    if (value.to !== undefined && !isClientName(value.to)) return null;
    if (typeof value.sentAt !== "number") return null;
    if (typeof value.index !== "number") return null;
    return value as BenchPayload;
  } catch {
    return null;
  }
}

function isClientName(value: unknown): value is ClientName {
  return value === "alice" || value === "bob" || value === "carol";
}

function createParticipant(name: ClientName, relay: InstrumentedRelay): Participant {
  const ownerPrivateKey = generateSecretKey();
  const ownerPubkey = getPublicKey(ownerPrivateKey);

  const nostrSubscribe: NostrSubscribe = (filter, onEvent) =>
    relay.subscribe(name, filter, onEvent).close;

  const nostrPublish: NostrPublish = async (
    event: UnsignedEvent | VerifiedEvent,
  ) => {
    const signed =
      "sig" in event && event.sig
        ? (event as VerifiedEvent)
        : (finalizeEvent(event as UnsignedEvent, ownerPrivateKey) as VerifiedEvent);
    relay.storeAndDeliver(name, signed);
    return signed;
  };

  return {
    name,
    ownerPubkey,
    runtime: new NdrRuntime({
      nostrSubscribe,
      nostrPublish,
      storage: new InMemoryStorageAdapter(),
      appKeysFastTimeoutMs: 50,
      appKeysFetchTimeoutMs: 500,
    }),
  };
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  message: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer) clearTimeout(timer);
  });
}

async function initializeParticipants(participants: Participant[]): Promise<void> {
  for (const participant of participants) {
    await participant.runtime.initForOwner(participant.ownerPubkey);
    await participant.runtime.registerCurrentDevice({
      ownerPubkey: participant.ownerPubkey,
      timeoutMs: 1_000,
    });
    await participant.runtime.republishInvite();
  }

  for (const participant of participants) {
    for (const peer of participants) {
      if (participant === peer) continue;
      await participant.runtime.setupUser(peer.ownerPubkey);
    }
  }
}

function buildPayload(input: {
  runId: string;
  scenario: Scenario;
  phase: "warmup" | "measure";
  from: ClientName;
  to?: ClientName;
  index: number;
}): BenchPayload {
  return {
    bench: BENCH_NAME,
    id: `${input.scenario}-${input.phase}-${input.from}-${input.to || "all"}-${input.index}`,
    sentAt: Date.now(),
    ...input,
  };
}

async function runDirectScenario(input: {
  runId: string;
  participants: Participant[];
  perDirection: number;
  timeoutMs: number;
  phase: "warmup" | "measure";
  metrics: DeliveryMetrics;
}): Promise<void> {
  const pending = new Map<
    string,
    {
      recipient: Participant;
      resolve: () => void;
    }
  >();

  const unsubscribes = input.participants.map((participant) =>
    participant.runtime.onSessionEvent((event, from) => {
      const payload = parsePayload(event.content);
      if (
        !payload ||
        payload.runId !== input.runId ||
        payload.scenario !== "direct" ||
        payload.phase !== input.phase
      ) {
        return;
      }

      const waiter = pending.get(payload.id);
      if (!waiter) {
        input.metrics.recordUnexpected();
        return;
      }

      if (
        participant.name !== waiter.recipient.name ||
        payload.to !== participant.name ||
        from !== input.participants.find((p) => p.name === payload.from)?.ownerPubkey
      ) {
        input.metrics.recordUnexpected();
        return;
      }

      input.metrics.recordDelivery(
        `${payload.id}:${participant.name}`,
        Date.now() - payload.sentAt,
      );
      pending.delete(payload.id);
      waiter.resolve();
    }),
  );

  try {
    const directedPairs = input.participants.flatMap((from) =>
      input.participants
        .filter((to) => to !== from)
        .map((to) => [from, to] as const),
    );
    let index = 0;
    for (let round = 0; round < input.perDirection; round += 1) {
      for (const [sender, recipient] of directedPairs) {
        const payload = buildPayload({
          runId: input.runId,
          scenario: "direct",
          phase: input.phase,
          from: sender.name,
          to: recipient.name,
          index,
        });
        index += 1;
        input.metrics.recordSend(1);
        const done = new Promise<void>((resolve) => {
          pending.set(payload.id, { recipient, resolve });
        });
        await sender.runtime.sendMessage(
          recipient.ownerPubkey,
          JSON.stringify(payload),
        );
        await withTimeout(
          done,
          input.timeoutMs,
          `direct delivery timed out for ${payload.id}`,
        ).catch((error) => {
          pending.delete(payload.id);
          input.metrics.recordTimeout(1);
          throw error;
        });
      }
    }
  } finally {
    for (const unsubscribe of unsubscribes) {
      unsubscribe();
    }
  }
}

async function runGroupScenario(input: {
  runId: string;
  participants: Participant[];
  perSender: number;
  timeoutMs: number;
  phase: "warmup" | "measure";
  metrics: DeliveryMetrics;
}): Promise<void> {
  const [alice, bob, carol] = input.participants;
  if (!alice || !bob || !carol) {
    throw new Error("group scenario requires exactly three participants");
  }

  const created = await alice.runtime.createGroup(
    "NDR relay churn bench",
    [bob.ownerPubkey, carol.ownerPubkey],
    { fanoutMetadata: false },
  );
  for (const participant of input.participants) {
    await participant.runtime.syncGroups([created.group], participant.ownerPubkey);
  }

  const pending = new Map<
    string,
    {
      expectedRecipients: Set<ClientName>;
      resolve: () => void;
    }
  >();

  const unsubscribes = input.participants.map((participant) =>
    participant.runtime.onGroupEvent((event) => {
      const payload = parsePayload(event.inner.content);
      if (
        !payload ||
        payload.runId !== input.runId ||
        payload.scenario !== "group" ||
        payload.phase !== input.phase
      ) {
        return;
      }

      const waiter = pending.get(payload.id);
      if (!waiter) {
        input.metrics.recordUnexpected();
        return;
      }

      if (!waiter.expectedRecipients.has(participant.name)) {
        input.metrics.recordUnexpected();
        return;
      }

      input.metrics.recordDelivery(
        `${payload.id}:${participant.name}`,
        Date.now() - payload.sentAt,
      );
      waiter.expectedRecipients.delete(participant.name);
      if (waiter.expectedRecipients.size === 0) {
        pending.delete(payload.id);
        waiter.resolve();
      }
    }),
  );

  try {
    let index = 0;
    for (let round = 0; round < input.perSender; round += 1) {
      for (const sender of input.participants) {
        const payload = buildPayload({
          runId: input.runId,
          scenario: "group",
          phase: input.phase,
          from: sender.name,
          index,
        });
        index += 1;
        const expectedRecipients = new Set<ClientName>(
          input.participants
            .filter((participant) => participant !== sender)
            .map((participant) => participant.name),
        );
        input.metrics.recordSend(expectedRecipients.size);
        const done = new Promise<void>((resolve) => {
          pending.set(payload.id, {
            expectedRecipients,
            resolve,
          });
        });
        await sender.runtime.sendGroupMessage(
          created.group.id,
          JSON.stringify(payload),
        );
        await withTimeout(
          done,
          input.timeoutMs,
          `group delivery timed out for ${payload.id}`,
        ).catch((error) => {
          const waiter = pending.get(payload.id);
          pending.delete(payload.id);
          input.metrics.recordTimeout(waiter?.expectedRecipients.size || 0);
          throw error;
        });
      }
    }
  } finally {
    for (const unsubscribe of unsubscribes) {
      unsubscribe();
    }
  }
}

function intFromEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) return fallback;
  return parsed;
}

function printScenario(name: string, metrics: ScenarioMetrics): void {
  console.log(
    `${name}: sent=${metrics.sent} received=${metrics.receivedDeliveries}/${metrics.expectedDeliveries} ` +
      `timeouts=${metrics.timedOutDeliveries} dupDecoded=${metrics.duplicateDecodedDeliveries} ` +
      `unexpected=${metrics.unexpectedDeliveries} latencyMs p50=${metrics.latency.p50Ms} ` +
      `p95=${metrics.latency.p95Ms} max=${metrics.latency.maxMs}`,
  );
}

function printRelayStats(stats: RelayStats): void {
  console.log(
    `relay: req=${stats.subscribeCalls} close=${stats.unsubscribeCalls} ` +
      `publish=${stats.publishCalls} replayed=${stats.replayedEvents} ` +
      `delivered=${stats.deliveredEvents} active=${stats.activeSubscriptions} ` +
      `maxActive=${stats.maxActiveSubscriptions} approxBytes out=${stats.publishBytes} ` +
      `sub=${stats.subscriptionBytes} in=${stats.deliveredBytes}`,
  );
  for (const [client, clientStats] of Object.entries(stats.byClient)) {
    console.log(
      `  ${client}: req=${clientStats.subscribeCalls} close=${clientStats.unsubscribeCalls} ` +
        `publish=${clientStats.publishCalls} replayed=${clientStats.replayedEvents} ` +
        `delivered=${clientStats.deliveredEvents}`,
    );
  }
}

async function main(): Promise<void> {
  const directPerDirection = intFromEnv("NDR_BENCH_DIRECT_PER_DIRECTION", 3);
  const groupPerSender = intFromEnv("NDR_BENCH_GROUP_PER_SENDER", 3);
  const timeoutMs = intFromEnv("NDR_BENCH_TIMEOUT_MS", 10_000);
  const runId = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const relay = new InstrumentedRelay();
  const participants: Participant[] = [
    createParticipant("alice", relay),
    createParticipant("bob", relay),
    createParticipant("carol", relay),
  ];

  console.log(
    `config: directPerDirection=${directPerDirection} groupPerSender=${groupPerSender} timeoutMs=${timeoutMs}`,
  );

  await initializeParticipants(participants);
  const warmupDirectMetrics = new DeliveryMetrics();
  await runDirectScenario({
    runId,
    participants,
    perDirection: 1,
    timeoutMs,
    phase: "warmup",
    metrics: warmupDirectMetrics,
  });

  await wait(1_700);
  relay.resetCounters();

  const directMetrics = new DeliveryMetrics();
  await runDirectScenario({
    runId,
    participants,
    perDirection: directPerDirection,
    timeoutMs,
    phase: "measure",
    metrics: directMetrics,
  });

  const groupMetrics = new DeliveryMetrics();
  await runGroupScenario({
    runId,
    participants,
    perSender: groupPerSender,
    timeoutMs,
    phase: "measure",
    metrics: groupMetrics,
  });

  await wait(1_700);
  const relayStats = relay.snapshot();
  const directSummary = directMetrics.summary();
  const groupSummary = groupMetrics.summary();
  const totalFailures =
    directSummary.timedOutDeliveries +
    directSummary.unexpectedDeliveries +
    groupSummary.timedOutDeliveries +
    groupSummary.unexpectedDeliveries;

  printScenario("direct", directSummary);
  printScenario("group", groupSummary);
  printRelayStats(relayStats);

  const result = {
    runId,
    config: {
      directPerDirection,
      groupPerSender,
      timeoutMs,
    },
    direct: directSummary,
    group: groupSummary,
    relay: relayStats,
    criticalFailures: totalFailures,
  };

  if (process.env.NDR_BENCH_JSON === "1") {
    console.log(JSON.stringify(result, null, 2));
  }

  if (totalFailures > 0) {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
