import { describe, expect, it } from "vitest";

import { classifyMessageOrigin, isCrossDeviceSelfOrigin, isSelfOrigin } from "../src/MessageOrigin";

describe("MessageOrigin", () => {
  const ourOwnerPubkey = "a".repeat(64);
  const ourDevicePubkey = "b".repeat(64);
  const otherOwnerPubkey = "c".repeat(64);
  const otherDevicePubkey = "d".repeat(64);

  it("classifies local-device origin", () => {
    const origin = classifyMessageOrigin({
      ourOwnerPubkey,
      ourDevicePubkey,
      senderOwnerPubkey: ourOwnerPubkey,
      senderDevicePubkey: ourDevicePubkey,
    });
    expect(origin).toBe("local-device");
    expect(isSelfOrigin(origin)).toBe(true);
    expect(isCrossDeviceSelfOrigin(origin)).toBe(false);
  });

  it("classifies same-owner-other-device origin", () => {
    const origin = classifyMessageOrigin({
      ourOwnerPubkey,
      ourDevicePubkey,
      senderOwnerPubkey: ourOwnerPubkey,
      senderDevicePubkey: otherDevicePubkey,
    });
    expect(origin).toBe("same-owner-other-device");
    expect(isSelfOrigin(origin)).toBe(true);
    expect(isCrossDeviceSelfOrigin(origin)).toBe(true);
  });

  it("classifies remote-owner origin", () => {
    const origin = classifyMessageOrigin({
      ourOwnerPubkey,
      ourDevicePubkey,
      senderOwnerPubkey: otherOwnerPubkey,
      senderDevicePubkey: otherDevicePubkey,
    });
    expect(origin).toBe("remote-owner");
    expect(isSelfOrigin(origin)).toBe(false);
    expect(isCrossDeviceSelfOrigin(origin)).toBe(false);
  });

  it("classifies unknown origin when provenance is insufficient", () => {
    const origin = classifyMessageOrigin({
      ourOwnerPubkey,
      ourDevicePubkey,
      senderOwnerPubkey: ourOwnerPubkey,
    });
    expect(origin).toBe("unknown");
    expect(isSelfOrigin(origin)).toBe(false);
    expect(isCrossDeviceSelfOrigin(origin)).toBe(false);
  });
});
