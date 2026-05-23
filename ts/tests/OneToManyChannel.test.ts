import { describe, expect, it } from "vitest";
import { getPublicKey, verifyEvent } from "nostr-tools";
import { hexToBytes } from "@noble/hashes/utils";

import { SenderKeyState } from "../src/SenderKey";
import { OneToManyChannel } from "../src/OneToManyChannel";
import { MESSAGE_EVENT_KIND } from "../src/types";
import { base64Decode } from "../src/base64";

describe("OneToManyChannel", () => {
  it("encrypts once and decrypts from outer payload", () => {
    const senderSk = hexToBytes("11".repeat(32));
    const senderPk = getPublicKey(senderSk);

    const keyId = 123;
    const chainKey = new Uint8Array(32).fill(7);
    const senderState = new SenderKeyState(keyId, chainKey, 0);
    const receiverState = new SenderKeyState(keyId, chainKey, 0);

    const channel = OneToManyChannel.default();
    const outer = channel.encryptToOuterEvent(
      senderSk,
      senderState,
      JSON.stringify({ kind: 14, content: "hello" }),
      1_700_000_000,
    );

    expect(outer.kind).toBe(MESSAGE_EVENT_KIND);
    expect(outer.pubkey).toBe(senderPk);
    expect(outer.tags.some((tag) => tag[0] === "header" && tag[1])).toBe(true);
    expect(verifyEvent(outer)).toBe(true);

    const publicCounterPrefix = new Uint8Array(8);
    const prefixView = new DataView(publicCounterPrefix.buffer);
    prefixView.setUint32(0, keyId, false);
    prefixView.setUint32(4, 0, false);
    expect(base64Decode(outer.content).subarray(0, 8)).not.toEqual(
      publicCounterPrefix,
    );

    const parsed = channel.parseOuterEvent(outer);
    expect(parsed.keyId).toBe(0);
    expect(parsed.messageNumber).toBe(0);
    expect(parsed.encryptedHeader).toBeDefined();

    const plaintext = parsed.decrypt(receiverState);
    expect(plaintext).toBe(JSON.stringify({ kind: 14, content: "hello" }));
  });
});
