import type { VerifiedEvent } from "nostr-tools";

import { Group, type GroupDecryptedEvent, type PairwiseSend, type PublishOuter } from "./GroupChannel";
import { GROUP_SENDER_KEY_DISTRIBUTION_KIND, type GroupData } from "./GroupMeta";
import { OneToManyChannel } from "./OneToManyChannel";
import type { SenderKeyDistribution } from "./SenderKey";
import { InMemoryStorageAdapter, type StorageAdapter } from "./StorageAdapter";
import { CHAT_MESSAGE_KIND, type NostrSubscribe, type Rumor, type Unsubscribe } from "./types";

export interface GroupManagerErrorContext {
  operation:
    | "upsertGroup"
    | "sendEvent"
    | "sendMessage"
    | "rotateSenderKey"
    | "handleIncomingSessionEvent"
    | "handleOuterEvent"
    | "syncOuterSubscription";
  groupId?: string;
  senderEventPubkey?: string;
  eventId?: string;
}

export interface GroupManagerOptions {
  ourOwnerPubkey: string;
  ourDevicePubkey: string;
  storage?: StorageAdapter;
  oneToMany?: OneToManyChannel;
  nostrSubscribe?: NostrSubscribe;
  onDecryptedEvent?: (event: GroupDecryptedEvent) => void;
  onError?: (error: unknown, context: GroupManagerErrorContext) => void;
}

function getFirstTagValue(tags: string[][] | undefined, key: string): string | undefined {
  const t = tags?.find((tag) => tag[0] === key);
  return t?.[1];
}

function isHex32(value: string): boolean {
  return typeof value === "string" && /^[0-9a-f]{64}$/i.test(value);
}

function parseSenderKeyDistribution(content: string): SenderKeyDistribution | null {
  try {
    const d = JSON.parse(content) as Partial<SenderKeyDistribution>;
    if (!d || typeof d !== "object") return null;
    if (typeof d.groupId !== "string") return null;
    if (typeof d.keyId !== "number" || !Number.isInteger(d.keyId) || d.keyId < 0) return null;
    if (typeof d.chainKey !== "string" || !/^[0-9a-f]{64}$/i.test(d.chainKey)) return null;
    if (typeof d.iteration !== "number" || !Number.isInteger(d.iteration) || d.iteration < 0) return null;
    if (typeof d.createdAt !== "number" || !Number.isInteger(d.createdAt) || d.createdAt < 0) return null;
    if (d.senderEventPubkey !== undefined && typeof d.senderEventPubkey !== "string") return null;
    return d as SenderKeyDistribution;
  } catch {
    return null;
  }
}

export class GroupManager {
  private readonly ourOwnerPubkey: string;
  private readonly ourDevicePubkey: string;
  private readonly storage: StorageAdapter;
  private readonly oneToMany: OneToManyChannel;
  private readonly nostrSubscribe?: NostrSubscribe;
  private readonly onDecryptedEvent?: (event: GroupDecryptedEvent) => void;
  private readonly onError?: (error: unknown, context: GroupManagerErrorContext) => void;

  private readonly groups = new Map<string, Group>();
  private readonly senderEventToGroup = new Map<string, string>();
  private readonly groupToSenderEvents = new Map<string, Set<string>>();
  private readonly pendingOuterBySenderEvent = new Map<string, VerifiedEvent[]>();

  private outerUnsubscribe: Unsubscribe | null = null;
  private outerAuthorsKey = "";
  private readonly maxPendingPerSenderEvent = 128;

  constructor(opts: GroupManagerOptions) {
    this.ourOwnerPubkey = opts.ourOwnerPubkey;
    this.ourDevicePubkey = opts.ourDevicePubkey;
    this.storage = opts.storage || new InMemoryStorageAdapter();
    this.oneToMany = opts.oneToMany || OneToManyChannel.default();
    this.nostrSubscribe = opts.nostrSubscribe;
    this.onDecryptedEvent = opts.onDecryptedEvent;
    this.onError = opts.onError;
  }

  async upsertGroup(data: GroupData): Promise<void> {
    const groupId = data.id;
    let group = this.groups.get(groupId);
    if (!group) {
      group = new Group({
        data,
        ourOwnerPubkey: this.ourOwnerPubkey,
        ourDevicePubkey: this.ourDevicePubkey,
        storage: this.storage,
        oneToMany: this.oneToMany,
      });
      this.groups.set(groupId, group);
    } else {
      group.setData(data);
    }

    await this.refreshGroupSenderMappings(groupId);
    await this.syncOuterSubscription();
  }

  removeGroup(groupId: string): void {
    this.groups.delete(groupId);

    const senderEvents = this.groupToSenderEvents.get(groupId);
    if (senderEvents) {
      for (const senderEventPubkey of senderEvents) {
        const mappedGroupId = this.senderEventToGroup.get(senderEventPubkey);
        if (mappedGroupId === groupId) {
          this.senderEventToGroup.delete(senderEventPubkey);
        }
      }
    }
    this.groupToSenderEvents.delete(groupId);

    void this.syncOuterSubscription();
  }

  destroy(): void {
    try {
      this.outerUnsubscribe?.();
    } catch {
      // ignore teardown errors
    }
    this.outerUnsubscribe = null;
    this.outerAuthorsKey = "";

    this.groups.clear();
    this.senderEventToGroup.clear();
    this.groupToSenderEvents.clear();
    this.pendingOuterBySenderEvent.clear();
  }

  async sendMessage(
    groupId: string,
    message: string,
    opts: { sendPairwise: PairwiseSend; publishOuter: PublishOuter; nowMs?: number }
  ): Promise<{ outer: VerifiedEvent; inner: Rumor }> {
    return this.sendEvent(
      groupId,
      {
        kind: CHAT_MESSAGE_KIND,
        content: message,
      },
      opts
    );
  }

  async sendEvent(
    groupId: string,
    event: { kind: number; content: string; tags?: string[][] },
    opts: { sendPairwise: PairwiseSend; publishOuter: PublishOuter; nowMs?: number }
  ): Promise<{ outer: VerifiedEvent; inner: Rumor }> {
    const group = this.groups.get(groupId);
    if (!group) {
      throw new Error(`Unknown group: ${groupId}`);
    }

    try {
      const result = await group.sendEvent(event, opts);
      await this.refreshGroupSenderMappings(groupId);
      await this.syncOuterSubscription();
      return result;
    } catch (error) {
      this.reportError(error, { operation: "sendEvent", groupId });
      throw error;
    }
  }

  async rotateSenderKey(
    groupId: string,
    opts: { sendPairwise: PairwiseSend; nowMs?: number }
  ): Promise<SenderKeyDistribution> {
    const group = this.groups.get(groupId);
    if (!group) {
      throw new Error(`Unknown group: ${groupId}`);
    }

    try {
      const result = await group.rotateSenderKey(opts);
      await this.refreshGroupSenderMappings(groupId);
      await this.syncOuterSubscription();
      return result;
    } catch (error) {
      this.reportError(error, { operation: "rotateSenderKey", groupId });
      throw error;
    }
  }

  async handleIncomingSessionEvent(
    event: Rumor,
    fromOwnerPubkey: string,
    fromSenderDevicePubkey?: string
  ): Promise<GroupDecryptedEvent[]> {
    const taggedGroupId = getFirstTagValue(event.tags, "l");
    let groupId = taggedGroupId;
    let distribution: SenderKeyDistribution | null = null;

    if (event.kind === GROUP_SENDER_KEY_DISTRIBUTION_KIND) {
      distribution = parseSenderKeyDistribution(event.content);
      if (distribution?.groupId) {
        groupId = distribution.groupId;
      }
    }

    if (!groupId) return [];
    const group = this.groups.get(groupId);
    if (!group) return [];

    try {
      const drainedFromGroup = await group.handleIncomingSessionEvent(
        event,
        fromOwnerPubkey,
        fromSenderDevicePubkey
      );

      const drainedFromManagerQueue: GroupDecryptedEvent[] = [];

      if (distribution?.senderEventPubkey && isHex32(distribution.senderEventPubkey)) {
        this.bindSenderEventToGroup(groupId, distribution.senderEventPubkey);
        const drained = await this.drainPendingOuterForSenderEvent(
          distribution.senderEventPubkey,
          group
        );
        drainedFromManagerQueue.push(...drained);
      }

      await this.refreshGroupSenderMappings(groupId);
      await this.syncOuterSubscription();

      const all = [...drainedFromGroup, ...drainedFromManagerQueue];
      this.emitDecryptedEvents(all);
      return all;
    } catch (error) {
      this.reportError(error, { operation: "handleIncomingSessionEvent", groupId, eventId: event.id });
      return [];
    }
  }

  async handleOuterEvent(outer: VerifiedEvent): Promise<GroupDecryptedEvent | null> {
    if (outer.kind !== this.oneToMany.outerEventKind()) return null;

    const senderEventPubkey = outer.pubkey;
    const groupId = this.senderEventToGroup.get(senderEventPubkey);
    if (!groupId) {
      this.queuePendingOuter(senderEventPubkey, outer);
      return null;
    }

    const group = this.groups.get(groupId);
    if (!group) {
      this.queuePendingOuter(senderEventPubkey, outer);
      return null;
    }

    try {
      const decrypted = await group.handleOuterEvent(outer);
      if (decrypted) {
        this.emitDecryptedEvents([decrypted]);
      }
      return decrypted;
    } catch (error) {
      this.reportError(error, {
        operation: "handleOuterEvent",
        groupId,
        senderEventPubkey,
        eventId: outer.id,
      });
      return null;
    }
  }

  async syncOuterSubscription(): Promise<void> {
    if (!this.nostrSubscribe) return;

    const authors = Array.from(this.senderEventToGroup.keys()).sort();
    const authorsKey = authors.join(",");
    if (authorsKey === this.outerAuthorsKey) return;

    try {
      this.outerUnsubscribe?.();
    } catch {
      // ignore teardown errors
    }
    this.outerUnsubscribe = null;
    this.outerAuthorsKey = authorsKey;

    if (authors.length === 0) return;

    try {
      this.outerUnsubscribe = this.nostrSubscribe(
        {
          kinds: [this.oneToMany.outerEventKind()],
          authors,
        },
        (event) => {
          void this.handleOuterEvent(event).catch((error) => {
            this.reportError(error, {
              operation: "handleOuterEvent",
              senderEventPubkey: event.pubkey,
              eventId: event.id,
            });
          });
        }
      );
    } catch (error) {
      this.reportError(error, { operation: "syncOuterSubscription" });
    }
  }

  private emitDecryptedEvents(events: GroupDecryptedEvent[]): void {
    if (!this.onDecryptedEvent) return;
    for (const event of events) {
      this.onDecryptedEvent(event);
    }
  }

  private bindSenderEventToGroup(groupId: string, senderEventPubkey: string): void {
    this.senderEventToGroup.set(senderEventPubkey, groupId);
    const current = this.groupToSenderEvents.get(groupId) || new Set<string>();
    current.add(senderEventPubkey);
    this.groupToSenderEvents.set(groupId, current);
  }

  private async refreshGroupSenderMappings(groupId: string): Promise<void> {
    const group = this.groups.get(groupId);
    if (!group) return;

    let nextSenderEvents: string[];
    try {
      nextSenderEvents = await group.listSenderEventPubkeys();
    } catch (error) {
      this.reportError(error, { operation: "upsertGroup", groupId });
      return;
    }

    const next = new Set(nextSenderEvents);
    const prev = this.groupToSenderEvents.get(groupId) || new Set<string>();

    for (const senderEventPubkey of prev) {
      if (next.has(senderEventPubkey)) continue;
      const mappedGroupId = this.senderEventToGroup.get(senderEventPubkey);
      if (mappedGroupId === groupId) {
        this.senderEventToGroup.delete(senderEventPubkey);
      }
    }

    for (const senderEventPubkey of next) {
      this.senderEventToGroup.set(senderEventPubkey, groupId);
    }

    this.groupToSenderEvents.set(groupId, next);
  }

  private queuePendingOuter(senderEventPubkey: string, outer: VerifiedEvent): void {
    const pending = this.pendingOuterBySenderEvent.get(senderEventPubkey) || [];
    if (pending.length >= this.maxPendingPerSenderEvent) {
      pending.shift();
    }
    pending.push(outer);
    this.pendingOuterBySenderEvent.set(senderEventPubkey, pending);
  }

  private async drainPendingOuterForSenderEvent(
    senderEventPubkey: string,
    group: Group
  ): Promise<GroupDecryptedEvent[]> {
    const pending = this.pendingOuterBySenderEvent.get(senderEventPubkey);
    if (!pending || pending.length === 0) return [];
    this.pendingOuterBySenderEvent.delete(senderEventPubkey);

    const withMessageNumber = pending
      .map((outer) => {
        try {
          const parsed = this.oneToMany.parseOuterContent(outer.content);
          return { outer, messageNumber: parsed.messageNumber };
        } catch {
          return { outer, messageNumber: 0 };
        }
      })
      .sort((a, b) => a.messageNumber - b.messageNumber);

    const decrypted: GroupDecryptedEvent[] = [];
    for (const { outer } of withMessageNumber) {
      const event = await group.handleOuterEvent(outer);
      if (event) decrypted.push(event);
    }
    return decrypted;
  }

  private reportError(error: unknown, context: GroupManagerErrorContext): void {
    this.onError?.(error, context);
  }
}
