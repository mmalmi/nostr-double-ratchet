import { finalizeEvent, VerifiedEvent } from "nostr-tools";

import { MESSAGE_EVENT_KIND } from "./types";
import { base64Decode, base64Encode } from "./base64";
import { SenderKeyState } from "./SenderKey";

/**
 * Parsed one-to-many outer payload.
 *
 * Outer content is: base64(keyId_be || messageNumber_be || nip44_ciphertext_bytes)
 */
export class OneToManyMessage {
  constructor(
    public readonly keyId: number,
    public readonly messageNumber: number,
    public readonly ciphertext: Uint8Array
  ) {}

  decrypt(state: SenderKeyState): string {
    return state.decryptFromBytes(this.messageNumber, this.ciphertext);
  }
}

/**
 * A lightweight helper for "one-to-many" publishing:
 *
 * - Outer Nostr event is authored by a sender-controlled pubkey (eg per-group sender keypair).
 * - Outer content is a compact base64 payload: `keyId_be || msgNum_be || nip44_ciphertext_bytes`.
 * - Ciphertext bytes are produced/consumed by SenderKeyState.
 */
export class OneToManyChannel {
  private readonly outerKind: number;

  constructor(outerKind: number = MESSAGE_EVENT_KIND) {
    this.outerKind = outerKind;
  }

  static default(): OneToManyChannel {
    return new OneToManyChannel(MESSAGE_EVENT_KIND);
  }

  outerEventKind(): number {
    return this.outerKind;
  }

  buildOuterContent(
    keyId: number,
    messageNumber: number,
    ciphertextBytes: Uint8Array
  ): string {
    const payload = new Uint8Array(8 + ciphertextBytes.length);
    const view = new DataView(payload.buffer);
    view.setUint32(0, keyId >>> 0, false);
    view.setUint32(4, messageNumber >>> 0, false);
    payload.set(ciphertextBytes, 8);
    return base64Encode(payload);
  }

  parseOuterContent(content: string): OneToManyMessage {
    const bytes = base64Decode(content);
    if (bytes.length < 8) {
      throw new Error("one-to-many payload too short");
    }

    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const keyId = view.getUint32(0, false);
    const messageNumber = view.getUint32(4, false);
    const ciphertext = bytes.subarray(8);
    return new OneToManyMessage(keyId, messageNumber, ciphertext);
  }

  encryptToOuterEvent(
    senderEventSecretKey: Uint8Array,
    senderKey: SenderKeyState,
    innerPlaintext: string,
    createdAt: number
  ): VerifiedEvent {
    const { messageNumber, ciphertext } = senderKey.encryptToBytes(innerPlaintext);
    const content = this.buildOuterContent(senderKey.keyId, messageNumber, ciphertext);

    return finalizeEvent(
      {
        kind: this.outerKind,
        content,
        tags: [],
        created_at: createdAt,
      },
      senderEventSecretKey
    );
  }
}

