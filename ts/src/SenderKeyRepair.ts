import { getEventHash } from "nostr-tools";

import {
  GROUP_SENDER_KEY_REPAIR_REQUEST_KIND,
} from "./GroupMeta";
import type { Rumor } from "./types";

export const SENDER_KEY_REPAIR_DEFAULT_RETRY_DELAYS_SECS = [
  30,
  120,
  600,
  3_600,
  21_600,
] as const;

export interface SenderKeyRepairRequest {
  groupId: string;
  senderEventPubkey: string;
  keyId: number;
  messageNumber: number;
  requiredRevision?: number;
  /** UNIX seconds */
  createdAt: number;
}

export interface SenderKeyRepairBlockedMessage {
  senderEventPubkey: string;
  keyId: number;
  messageNumber: number;
}

export type SenderKeyRepairHandleResult =
  | {
      type: "pending_distribution";
      groupId: string;
      senderEventPubkey: string;
      keyId: number;
    }
  | {
      type: "pending_revision";
      groupId: string;
      currentRevision: number;
      requiredRevision: number;
    }
  | { type: "event" }
  | { type: "ignored" };

function getFirstTagValue(tags: string[][] | undefined, key: string): string | undefined {
  const tag = tags?.find((entry) => entry[0] === key);
  return tag?.[1];
}

function isHex32(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/i.test(value);
}

function isU32(value: unknown): value is number {
  return Number.isInteger(value) && (value as number) >= 0 && (value as number) <= 0xffff_ffff;
}

function isSafeNonNegativeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

export function senderKeyRepairDefaultRetryDelaySeconds(sentRequestCount: number): number {
  const count = Math.max(0, Math.floor(sentRequestCount));
  if (count <= 1) return SENDER_KEY_REPAIR_DEFAULT_RETRY_DELAYS_SECS[0];
  const index = Math.min(count - 1, SENDER_KEY_REPAIR_DEFAULT_RETRY_DELAYS_SECS.length - 1);
  return SENDER_KEY_REPAIR_DEFAULT_RETRY_DELAYS_SECS[index];
}

export function senderKeyRepairDefaultNextRetryAt(
  nowSeconds: number,
  sentRequestCount: number,
): number {
  const now = Math.max(0, Math.floor(nowSeconds));
  const delay = senderKeyRepairDefaultRetryDelaySeconds(sentRequestCount);
  const next = now + delay;
  return Number.isSafeInteger(next) ? next : Number.MAX_SAFE_INTEGER;
}

export function senderKeyRepairRequestFromPendingSenderKeyMessage(
  message: SenderKeyRepairBlockedMessage,
  result: SenderKeyRepairHandleResult,
  createdAt: number,
): SenderKeyRepairRequest | null {
  if (result.type === "pending_distribution") {
    return {
      groupId: result.groupId,
      senderEventPubkey: result.senderEventPubkey,
      keyId: result.keyId,
      messageNumber: message.messageNumber >>> 0,
      createdAt: Math.max(0, Math.floor(createdAt)),
    };
  }

  if (result.type === "pending_revision") {
    return {
      groupId: result.groupId,
      senderEventPubkey: message.senderEventPubkey,
      keyId: message.keyId >>> 0,
      messageNumber: message.messageNumber >>> 0,
      requiredRevision: Math.max(0, Math.floor(result.requiredRevision)),
      createdAt: Math.max(0, Math.floor(createdAt)),
    };
  }

  return null;
}

export function buildSenderKeyRepairRequestRumor(
  request: SenderKeyRepairRequest,
  senderDevicePubkey: string,
  nowMs?: number,
): Rumor {
  const createdAtMs = nowMs ?? request.createdAt * 1000;
  const createdAtSeconds = Math.floor(createdAtMs / 1000);
  const tags = [
    ["l", request.groupId],
    ["key", String(request.keyId >>> 0)],
    ["sender", request.senderEventPubkey],
    ["message", String(request.messageNumber >>> 0)],
    ["ms", String(createdAtMs)],
  ];
  if (request.requiredRevision !== undefined) {
    tags.push(["revision", String(Math.max(0, Math.floor(request.requiredRevision)))]);
  }

  const rumor: Rumor = {
    kind: GROUP_SENDER_KEY_REPAIR_REQUEST_KIND,
    content: JSON.stringify({
      groupId: request.groupId,
      senderEventPubkey: request.senderEventPubkey,
      keyId: request.keyId >>> 0,
      messageNumber: request.messageNumber >>> 0,
      ...(request.requiredRevision !== undefined
        ? { requiredRevision: Math.max(0, Math.floor(request.requiredRevision)) }
        : {}),
      createdAt: Math.max(0, Math.floor(request.createdAt)),
    }),
    created_at: createdAtSeconds,
    tags,
    pubkey: senderDevicePubkey,
    id: "",
  };
  rumor.id = getEventHash(rumor);
  return rumor;
}

export function parseSenderKeyRepairRequestRumor(event: Rumor): SenderKeyRepairRequest | null {
  if (event.kind !== GROUP_SENDER_KEY_REPAIR_REQUEST_KIND) return null;
  if (event.id && getEventHash(event) !== event.id) return null;

  let parsed: Partial<SenderKeyRepairRequest>;
  try {
    parsed = JSON.parse(event.content) as Partial<SenderKeyRepairRequest>;
  } catch {
    return null;
  }

  if (typeof parsed.groupId !== "string" || parsed.groupId.length === 0) return null;
  if (!isHex32(parsed.senderEventPubkey)) return null;
  if (!isU32(parsed.keyId)) return null;
  if (!isU32(parsed.messageNumber)) return null;
  if (!isSafeNonNegativeInteger(parsed.createdAt)) return null;
  if (
    parsed.requiredRevision !== undefined &&
    !isSafeNonNegativeInteger(parsed.requiredRevision)
  ) {
    return null;
  }

  if (getFirstTagValue(event.tags, "l") !== parsed.groupId) return null;
  if (getFirstTagValue(event.tags, "key") !== String(parsed.keyId >>> 0)) return null;
  if (getFirstTagValue(event.tags, "sender") !== parsed.senderEventPubkey) return null;
  if (getFirstTagValue(event.tags, "message") !== String(parsed.messageNumber >>> 0)) return null;

  const revisionTag = getFirstTagValue(event.tags, "revision");
  if (parsed.requiredRevision === undefined) {
    if (revisionTag !== undefined) return null;
  } else if (revisionTag !== String(Math.max(0, Math.floor(parsed.requiredRevision)))) {
    return null;
  }

  return {
    groupId: parsed.groupId,
    senderEventPubkey: parsed.senderEventPubkey,
    keyId: parsed.keyId >>> 0,
    messageNumber: parsed.messageNumber >>> 0,
    ...(parsed.requiredRevision !== undefined
      ? { requiredRevision: Math.max(0, Math.floor(parsed.requiredRevision)) }
      : {}),
    createdAt: Math.max(0, Math.floor(parsed.createdAt)),
  };
}
