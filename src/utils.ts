import { hexToBytes, bytesToHex } from "@noble/hashes/utils";
import { ChannelState, Message } from "./types";
import { Channel } from "./Channel";
import { extract as hkdf_extract, expand as hkdf_expand } from '@noble/hashes/hkdf';
import { sha256 } from '@noble/hashes/sha256';

export function serializeChannelState(state: ChannelState): string {
  return JSON.stringify({
    theirCurrentNostrPublicKey: state.theirCurrentNostrPublicKey,
    ourCurrentNostrKey: {
      publicKey: state.ourCurrentNostrKey.publicKey,
      privateKey: bytesToHex(state.ourCurrentNostrKey.privateKey),
    },
    ourNextNostrKey: {
      publicKey: state.ourNextNostrKey.publicKey,
      privateKey: bytesToHex(state.ourNextNostrKey.privateKey),
    },
    receivingChainKey: bytesToHex(state.receivingChainKey),
    nextReceivingChainKey: bytesToHex(state.nextReceivingChainKey),
    sendingChainKey: bytesToHex(state.sendingChainKey),
    sendingChainMessageNumber: state.sendingChainMessageNumber,
    receivingChainMessageNumber: state.receivingChainMessageNumber,
    previousSendingChainMessageCount: state.previousSendingChainMessageCount,
    skippedMessageKeys: Object.fromEntries(
      Object.entries(state.skippedMessageKeys).map(([key, value]) => [
        key,
        bytesToHex(value),
      ])
    ),
  });
}

export function deserializeChannelState(data: string): ChannelState {
  const state = JSON.parse(data);
  return {
    theirCurrentNostrPublicKey: state.theirCurrentNostrPublicKey,
    ourCurrentNostrKey: {
      publicKey: state.ourCurrentNostrKey.publicKey,
      privateKey: hexToBytes(state.ourCurrentNostrKey.privateKey),
    },
    ourNextNostrKey: {
      publicKey: state.ourNextNostrKey.publicKey,
      privateKey: hexToBytes(state.ourNextNostrKey.privateKey),
    },
    receivingChainKey: hexToBytes(state.receivingChainKey),
    nextReceivingChainKey: hexToBytes(state.nextReceivingChainKey),
    sendingChainKey: hexToBytes(state.sendingChainKey),
    sendingChainMessageNumber: state.sendingChainMessageNumber,
    receivingChainMessageNumber: state.receivingChainMessageNumber,
    previousSendingChainMessageCount: state.previousSendingChainMessageCount,
    skippedMessageKeys: Object.fromEntries(
      Object.entries(state.skippedMessageKeys).map(([key, value]) => [
        key,
        hexToBytes(value as string),
      ])
    ),
  };
}

export async function* createMessageStream(channel: Channel): AsyncGenerator<Message, void, unknown> {
  const messageQueue: Message[] = [];
  let resolveNext: ((value: Message) => void) | null = null;

  const unsubscribe = channel.onMessage((message) => {
    if (resolveNext) {
      resolveNext(message);
      resolveNext = null;
    } else {
      messageQueue.push(message);
    }
  });

  try {
    while (true) {
      if (messageQueue.length > 0) {
        yield messageQueue.shift()!;
      } else {
        yield new Promise<Message>(resolve => {
          resolveNext = resolve;
        });
      }
    }
  } finally {
    unsubscribe();
  }
}

export function kdf(input1: Uint8Array, input2: Uint8Array = new Uint8Array(32)) {
  const prk = hkdf_extract(sha256, input1, input2)
  return hkdf_expand(sha256, prk, new Uint8Array([1]), 32)
}
