export type MessageOrigin =
  | "local-device"
  | "same-owner-other-device"
  | "remote-owner"
  | "unknown";

export interface MessageOriginInput {
  ourOwnerPubkey: string;
  ourDevicePubkey?: string;
  senderOwnerPubkey?: string;
  senderDevicePubkey?: string;
}

export function classifyMessageOrigin(input: MessageOriginInput): MessageOrigin {
  const {
    ourOwnerPubkey,
    ourDevicePubkey,
    senderOwnerPubkey,
    senderDevicePubkey,
  } = input;

  if (senderOwnerPubkey) {
    if (senderOwnerPubkey !== ourOwnerPubkey) return "remote-owner";
    if (!ourDevicePubkey || !senderDevicePubkey) return "unknown";
    return senderDevicePubkey === ourDevicePubkey
      ? "local-device"
      : "same-owner-other-device";
  }

  if (!ourDevicePubkey || !senderDevicePubkey) return "unknown";
  if (senderDevicePubkey === ourDevicePubkey) return "local-device";
  return "unknown";
}

export function isSelfOrigin(origin: MessageOrigin): boolean {
  return origin === "local-device" || origin === "same-owner-other-device";
}

export function isCrossDeviceSelfOrigin(origin: MessageOrigin): boolean {
  return origin === "same-owner-other-device";
}
