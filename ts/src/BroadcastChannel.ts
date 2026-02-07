import { bytesToHex, hexToBytes } from "@noble/hashes/utils";
import { generateSecretKey, getEventHash, getPublicKey, verifyEvent, type VerifiedEvent } from "nostr-tools";

import { GROUP_SENDER_KEY_DISTRIBUTION_KIND } from "./Group";
import { OneToManyChannel } from "./OneToManyChannel";
import type { SenderKeyDistribution, SenderKeyStateSerialized } from "./SenderKey";
import { SenderKeyState } from "./SenderKey";
import { InMemoryStorageAdapter, type StorageAdapter } from "./StorageAdapter";
import { CHAT_MESSAGE_KIND, MESSAGE_EVENT_KIND, type Rumor } from "./types";

export type PairwiseSend = (recipientOwnerPubkey: string, rumor: Rumor) => Promise<void>;
export type PublishOuter = (outer: VerifiedEvent) => Promise<unknown>;

export interface BroadcastChannelOptions {
  groupId: string;
  /** Owner pubkey for *this* device (group membership is expressed in owner pubkeys). */
  ourOwnerPubkey: string;
  /** Device identity pubkey for *this* device (used inside encrypted payloads). */
  ourDevicePubkey: string;
  memberOwnerPubkeys: string[];
  storage?: StorageAdapter;
  oneToMany?: OneToManyChannel;
}

export interface BroadcastDecryptedEvent {
  groupId: string;
  senderEventPubkey: string;
  senderDevicePubkey: string;
  senderOwnerPubkey?: string;
  outerEventId: string;
  outerCreatedAt: number;
  keyId: number;
  messageNumber: number;
  inner: Rumor;
}

function getFirstTagValue(tags: string[][] | undefined, key: string): string | undefined {
  const t = tags?.find((tag) => tag[0] === key);
  return t?.[1];
}

function randomU32(): number {
  const buf = new Uint32Array(1);
  crypto.getRandomValues(buf);
  return buf[0] >>> 0;
}

function isHex32(s: string): boolean {
  return typeof s === "string" && /^[0-9a-f]{64}$/i.test(s);
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

/**
 * Signal-style efficient group broadcast:
 *
 * - Each *device* has a per-group "sender event" pubkey (outer Nostr author).
 * - Each device uses a symmetric SenderKeyState chain for forward secrecy within that sender.
 * - Sender keys are distributed pairwise over 1:1 Double Ratchet sessions (forward secure).
 * - Group messages are published once using OneToManyChannel.
 *
 * This class is intentionally transport-agnostic:
 * - You provide `sendPairwise` for distributing keys over 1:1 sessions.
 * - You provide `publishOuter` for publishing the one-to-many outer events.
 * - You feed it incoming session rumors + outer events via the `handle…` methods.
 */
export class BroadcastChannel {
  private readonly groupId: string;
  private readonly ourOwnerPubkey: string;
  private readonly ourDevicePubkey: string;
  private memberOwnerPubkeys: string[];
  private readonly storage: StorageAdapter;
  private readonly oneToMany: OneToManyChannel;

  private readonly storageVersion = "1";
  private readonly versionPrefix: string;

  private initialized = false;

  // In-memory maps (rebuilt from storage on init, then updated live)
  private senderDeviceToEvent: Map<string, string> = new Map();
  private senderEventToDevice: Map<string, string> = new Map();
  private senderDeviceToOwner: Map<string, string> = new Map();

  // Outer events we couldn't decrypt yet (waiting for mapping/state).
  private pendingOuter: Map<string, VerifiedEvent[]> = new Map(); // key: `${senderEventPubkey}:${keyId}`

  constructor(opts: BroadcastChannelOptions) {
    this.groupId = opts.groupId;
    this.ourOwnerPubkey = opts.ourOwnerPubkey;
    this.ourDevicePubkey = opts.ourDevicePubkey;
    this.memberOwnerPubkeys = [...opts.memberOwnerPubkeys];
    this.storage = opts.storage || new InMemoryStorageAdapter();
    this.oneToMany = opts.oneToMany || OneToManyChannel.default();
    this.versionPrefix = `v${this.storageVersion}/broadcast-channel`;
  }

  setMembers(memberOwnerPubkeys: string[]): void {
    this.memberOwnerPubkeys = [...memberOwnerPubkeys];
  }

  private async init(): Promise<void> {
    if (this.initialized) return;
    this.initialized = true;

    // Load (device -> owner) and (device -> senderEventPubkey) mappings.
    const groupPrefix = `${this.versionPrefix}/group/${this.groupId}/sender/`;
    const keys = await this.storage.list(groupPrefix);

    for (const key of keys) {
      if (key.endsWith("/sender-event-pubkey")) {
        const senderDevicePubkey = key
          .slice(groupPrefix.length)
          .split("/")[0];
        const senderEventPubkey = await this.storage.get<string>(key);
        if (typeof senderEventPubkey === "string" && isHex32(senderEventPubkey) && isHex32(senderDevicePubkey)) {
          this.setSenderEventMapping(senderDevicePubkey, senderEventPubkey);
        }
      } else if (key.endsWith("/sender-owner-pubkey")) {
        const senderDevicePubkey = key
          .slice(groupPrefix.length)
          .split("/")[0];
        const owner = await this.storage.get<string>(key);
        if (typeof owner === "string" && isHex32(senderDevicePubkey)) {
          this.senderDeviceToOwner.set(senderDevicePubkey, owner);
        }
      }
    }
  }

  private groupSenderPrefix(senderDevicePubkey: string): string {
    return `${this.versionPrefix}/group/${this.groupId}/sender/${senderDevicePubkey}`;
  }

  private senderEventSecretKeyKey(senderDevicePubkey: string): string {
    return `${this.groupSenderPrefix(senderDevicePubkey)}/sender-event-secret-key`;
  }

  private senderEventPubkeyKey(senderDevicePubkey: string): string {
    return `${this.groupSenderPrefix(senderDevicePubkey)}/sender-event-pubkey`;
  }

  private senderOwnerPubkeyKey(senderDevicePubkey: string): string {
    return `${this.groupSenderPrefix(senderDevicePubkey)}/sender-owner-pubkey`;
  }

  private latestKeyIdKey(senderDevicePubkey: string): string {
    return `${this.groupSenderPrefix(senderDevicePubkey)}/latest-key-id`;
  }

  private senderKeyStateKey(senderDevicePubkey: string, keyId: number): string {
    return `${this.groupSenderPrefix(senderDevicePubkey)}/key/${keyId >>> 0}`;
  }

  private setSenderEventMapping(senderDevicePubkey: string, senderEventPubkey: string): void {
    const prev = this.senderDeviceToEvent.get(senderDevicePubkey);
    if (prev && prev !== senderEventPubkey) {
      this.senderEventToDevice.delete(prev);
    }
    this.senderDeviceToEvent.set(senderDevicePubkey, senderEventPubkey);
    this.senderEventToDevice.set(senderEventPubkey, senderDevicePubkey);
  }

  private pendingKey(senderEventPubkey: string, keyId: number): string {
    return `${senderEventPubkey}:${keyId >>> 0}`;
  }

  private queuePending(senderEventPubkey: string, keyId: number, outer: VerifiedEvent): void {
    const k = this.pendingKey(senderEventPubkey, keyId);
    const existing = this.pendingOuter.get(k) || [];
    existing.push(outer);
    this.pendingOuter.set(k, existing);
  }

  private async drainPending(senderEventPubkey: string, keyId: number): Promise<BroadcastDecryptedEvent[]> {
    const k = this.pendingKey(senderEventPubkey, keyId);
    const pending = this.pendingOuter.get(k);
    if (!pending || pending.length === 0) return [];
    this.pendingOuter.delete(k);

    // Best-effort: decrypt in message-number order to reduce skipped-key cache pressure.
    const withN = pending
      .map((outer) => {
        try {
          const parsed = this.oneToMany.parseOuterContent(outer.content);
          return { outer, n: parsed.messageNumber };
        } catch {
          return { outer, n: 0 };
        }
      })
      .sort((a, b) => a.n - b.n);

    const results: BroadcastDecryptedEvent[] = [];
    for (const { outer } of withN) {
      const dec = await this.handleOuterEvent(outer);
      if (dec) results.push(dec);
    }
    return results;
  }

  private async ensureOurSenderEventKeys(): Promise<{
    senderEventSecretKey: Uint8Array;
    senderEventPubkey: string;
    changed: boolean;
  }> {
    await this.init();

    const stored = await this.storage.get<string>(this.senderEventSecretKeyKey(this.ourDevicePubkey));
    if (typeof stored === "string" && /^[0-9a-f]{64}$/i.test(stored)) {
      const bytes = hexToBytes(stored);
      if (bytes.length === 32) {
        const senderEventPubkey = getPublicKey(bytes);

        // Keep a cached mapping so we can subscribe/decrypt our own outer events if needed.
        this.setSenderEventMapping(this.ourDevicePubkey, senderEventPubkey);
        await this.storage.put(this.senderEventPubkeyKey(this.ourDevicePubkey), senderEventPubkey);

        return { senderEventSecretKey: bytes, senderEventPubkey, changed: false };
      }
    }

    // Missing/invalid: rotate to a fresh sender-event keypair for this group/device.
    const senderEventSecretKey = generateSecretKey();
    const senderEventPubkey = getPublicKey(senderEventSecretKey);
    await this.storage.put(this.senderEventSecretKeyKey(this.ourDevicePubkey), bytesToHex(senderEventSecretKey));
    await this.storage.put(this.senderEventPubkeyKey(this.ourDevicePubkey), senderEventPubkey);
    this.setSenderEventMapping(this.ourDevicePubkey, senderEventPubkey);
    return { senderEventSecretKey, senderEventPubkey, changed: true };
  }

  private async loadSenderKeyState(senderDevicePubkey: string, keyId: number): Promise<SenderKeyState | null> {
    const data = await this.storage.get<SenderKeyStateSerialized>(this.senderKeyStateKey(senderDevicePubkey, keyId));
    if (!data) return null;
    try {
      return SenderKeyState.fromJSON(data);
    } catch {
      return null;
    }
  }

  private async saveSenderKeyState(senderDevicePubkey: string, st: SenderKeyState): Promise<void> {
    await this.storage.put(this.senderKeyStateKey(senderDevicePubkey, st.keyId), st.toJSON());
  }

  private async ensureOurSenderKeyState(forceRotate: boolean): Promise<{ state: SenderKeyState; created: boolean }> {
    await this.init();

    if (forceRotate) {
      const keyId = randomU32();
      const chainKey = generateSecretKey();
      const state = new SenderKeyState(keyId, chainKey, 0);
      await this.saveSenderKeyState(this.ourDevicePubkey, state);
      await this.storage.put(this.latestKeyIdKey(this.ourDevicePubkey), keyId);
      return { state, created: true };
    }

    const latestKeyId = await this.storage.get<number>(this.latestKeyIdKey(this.ourDevicePubkey));
    if (typeof latestKeyId === "number" && Number.isInteger(latestKeyId) && latestKeyId >= 0) {
      const existing = await this.loadSenderKeyState(this.ourDevicePubkey, latestKeyId >>> 0);
      if (existing) {
        return { state: existing, created: false };
      }
    }

    // Missing/invalid: create a fresh sender key state.
    const keyId = randomU32();
    const chainKey = generateSecretKey();
    const state = new SenderKeyState(keyId, chainKey, 0);
    await this.saveSenderKeyState(this.ourDevicePubkey, state);
    await this.storage.put(this.latestKeyIdKey(this.ourDevicePubkey), keyId);
    return { state, created: true };
  }

  private buildDistribution(nowSeconds: number, senderEventPubkey: string, senderKey: SenderKeyState): SenderKeyDistribution {
    return {
      groupId: this.groupId,
      keyId: senderKey.keyId,
      chainKey: bytesToHex(senderKey.chainKeyBytes()),
      iteration: senderKey.iterationNumber(),
      createdAt: nowSeconds,
      senderEventPubkey,
    };
  }

  private buildDistributionRumor(nowSeconds: number, nowMs: number, dist: SenderKeyDistribution): Rumor {
    const rumor: Rumor = {
      kind: GROUP_SENDER_KEY_DISTRIBUTION_KIND,
      content: JSON.stringify(dist),
      created_at: nowSeconds,
      tags: [
        ["l", this.groupId],
        ["key", String(dist.keyId >>> 0)],
        ["ms", String(nowMs)],
      ],
      pubkey: this.ourDevicePubkey,
      id: "",
    };
    rumor.id = getEventHash(rumor);
    return rumor;
  }

  private buildGroupInnerRumor(nowSeconds: number, nowMs: number, message: string): Rumor {
    const rumor: Rumor = {
      kind: CHAT_MESSAGE_KIND,
      content: message,
      created_at: nowSeconds,
      tags: [
        ["l", this.groupId],
        ["ms", String(nowMs)],
      ],
      pubkey: this.ourDevicePubkey,
      id: "",
    };
    rumor.id = getEventHash(rumor);
    return rumor;
  }

  /**
   * Rotate our sender key (new keyId + chain key) and distribute it to group members.
   */
  async rotateSenderKey(opts: { sendPairwise: PairwiseSend; nowMs?: number }): Promise<SenderKeyDistribution> {
    await this.init();

    const nowMs = opts.nowMs ?? Date.now();
    const nowSeconds = Math.floor(nowMs / 1000);

    const { senderEventPubkey } = await this.ensureOurSenderEventKeys();
    const { state } = await this.ensureOurSenderKeyState(true);

    const dist = this.buildDistribution(nowSeconds, senderEventPubkey, state);
    const rumor = this.buildDistributionRumor(nowSeconds, nowMs, dist);

    // Best-effort fanout (skip ourselves).
    await Promise.allSettled(
      this.memberOwnerPubkeys
        .filter((pk) => pk !== this.ourOwnerPubkey)
        .map((pk) => opts.sendPairwise(pk, rumor))
    );

    return dist;
  }

  /**
   * Send a group message:
   * - Ensures we have sender-event keys and a sender key state.
   * - Distributes sender key to group members if needed.
   * - Publishes exactly one outer event via OneToManyChannel.
   */
  async sendMessage(
    message: string,
    opts: { sendPairwise: PairwiseSend; publishOuter: PublishOuter; nowMs?: number }
  ): Promise<{ outer: VerifiedEvent; inner: Rumor }> {
    await this.init();

    const nowMs = opts.nowMs ?? Date.now();
    const nowSeconds = Math.floor(nowMs / 1000);

    const { senderEventSecretKey, senderEventPubkey, changed: senderEventKeysChanged } =
      await this.ensureOurSenderEventKeys();
    const { state: senderKey, created: senderKeyCreated } = await this.ensureOurSenderKeyState(false);

    // Distribute if we just created the sender key, or if our sender-event pubkey changed.
    if (senderKeyCreated || senderEventKeysChanged) {
      const dist = this.buildDistribution(nowSeconds, senderEventPubkey, senderKey);
      const rumor = this.buildDistributionRumor(nowSeconds, nowMs, dist);
      await Promise.allSettled(
        this.memberOwnerPubkeys
          .filter((pk) => pk !== this.ourOwnerPubkey)
          .map((pk) => opts.sendPairwise(pk, rumor))
      );
    }

    const inner = this.buildGroupInnerRumor(nowSeconds, nowMs, message);
    const innerJson = JSON.stringify(inner);
    const outer = this.oneToMany.encryptToOuterEvent(senderEventSecretKey, senderKey, innerJson, nowSeconds);

    await this.saveSenderKeyState(this.ourDevicePubkey, senderKey);
    await opts.publishOuter(outer);

    return { outer, inner };
  }

  /**
   * Handle an incoming 1:1 session rumor (decrypted Double Ratchet event).
   *
   * Currently this only consumes sender-key distribution rumors (kind 10446) for this group.
   * It may return decrypted outer group events that were pending until the distribution arrived.
   */
  async handleIncomingSessionEvent(event: Rumor, fromOwnerPubkey: string): Promise<BroadcastDecryptedEvent[]> {
    await this.init();

    if (!this.memberOwnerPubkeys.includes(fromOwnerPubkey)) {
      return [];
    }

    const gid = getFirstTagValue(event.tags, "l");
    if (gid !== this.groupId) return [];

    if (event.kind !== GROUP_SENDER_KEY_DISTRIBUTION_KIND) return [];

    const dist = parseSenderKeyDistribution(event.content);
    if (!dist) return [];
    if (dist.groupId !== this.groupId) return [];

    const senderDevicePubkey = event.pubkey;
    if (!isHex32(senderDevicePubkey)) return [];

    // Persist sender->owner mapping (for UI attribution).
    this.senderDeviceToOwner.set(senderDevicePubkey, fromOwnerPubkey);
    await this.storage.put(this.senderOwnerPubkeyKey(senderDevicePubkey), fromOwnerPubkey);

    // Learn/update sender-event pubkey mapping (used to route outer messages).
    if (dist.senderEventPubkey && isHex32(dist.senderEventPubkey)) {
      this.setSenderEventMapping(senderDevicePubkey, dist.senderEventPubkey);
      await this.storage.put(this.senderEventPubkeyKey(senderDevicePubkey), dist.senderEventPubkey);
    }

    // Store sender key state for this key id if we don't already have one.
    const existing = await this.storage.get<SenderKeyStateSerialized>(this.senderKeyStateKey(senderDevicePubkey, dist.keyId));
    if (!existing) {
      const st = SenderKeyState.fromDistribution(dist);
      await this.saveSenderKeyState(senderDevicePubkey, st);
    }

    // If we have a sender-event pubkey, retry pending outer events for (senderEventPubkey,keyId).
    if (dist.senderEventPubkey && isHex32(dist.senderEventPubkey)) {
      return await this.drainPending(dist.senderEventPubkey, dist.keyId);
    }

    return [];
  }

  /**
   * Handle an incoming one-to-many outer event (authored by a per-sender sender-event pubkey).
   *
   * Returns a decrypted inner rumor if possible, or null if we're missing mapping/state and queued it.
   */
  async handleOuterEvent(outer: VerifiedEvent): Promise<BroadcastDecryptedEvent | null> {
    await this.init();

    if (outer.kind !== MESSAGE_EVENT_KIND) return null;
    if (!verifyEvent(outer)) return null;

    let parsed: { keyId: number; messageNumber: number; ciphertext: Uint8Array };
    try {
      const p = this.oneToMany.parseOuterContent(outer.content);
      parsed = { keyId: p.keyId, messageNumber: p.messageNumber, ciphertext: p.ciphertext };
    } catch {
      return null;
    }

    const senderEventPubkey = outer.pubkey;
    const senderDevicePubkey = this.senderEventToDevice.get(senderEventPubkey);
    if (!senderDevicePubkey) {
      this.queuePending(senderEventPubkey, parsed.keyId, outer);
      return null;
    }

    const st = await this.loadSenderKeyState(senderDevicePubkey, parsed.keyId);
    if (!st) {
      this.queuePending(senderEventPubkey, parsed.keyId, outer);
      return null;
    }

    let plaintext: string;
    try {
      plaintext = st.decryptFromBytes(parsed.messageNumber, parsed.ciphertext);
    } catch {
      return null;
    }

    await this.saveSenderKeyState(senderDevicePubkey, st);

    let inner: Rumor;
    try {
      inner = JSON.parse(plaintext) as Rumor;
    } catch {
      // Not a Nostr JSON event; wrap as a message-shaped rumor.
      inner = {
        kind: CHAT_MESSAGE_KIND,
        content: plaintext,
        created_at: outer.created_at,
        tags: [["l", this.groupId]],
        pubkey: senderDevicePubkey,
        id: "",
      };
      inner.id = getEventHash(inner);
    }

    // Best-effort sanity: ensure this is the right group (encrypted, so shouldn't leak).
    const innerGid = getFirstTagValue(inner.tags, "l");
    if (innerGid && innerGid !== this.groupId) {
      return null;
    }

    const senderOwnerPubkey =
      this.senderDeviceToOwner.get(senderDevicePubkey) ||
      (await this.storage.get<string>(this.senderOwnerPubkeyKey(senderDevicePubkey))) ||
      undefined;

    return {
      groupId: this.groupId,
      senderEventPubkey,
      senderDevicePubkey,
      senderOwnerPubkey,
      outerEventId: outer.id,
      outerCreatedAt: outer.created_at,
      keyId: parsed.keyId,
      messageNumber: parsed.messageNumber,
      inner,
    };
  }
}
