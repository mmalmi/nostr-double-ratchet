import { hexToBytes, bytesToHex } from "@noble/hashes/utils";
import { ChannelState, Message } from "./types";
import { Channel } from "./Channel";
import { extract as hkdf_extract, expand as hkdf_expand } from '@noble/hashes/hkdf';
import { sha256 } from '@noble/hashes/sha256';

/**
 * Serialize a channel state to a string.
 */
export function serializeChannelState(state: ChannelState): string {
  return JSON.stringify({
    rootKey: bytesToHex(state.rootKey),
    theirNostrPublicKey: state.theirNostrPublicKey,
    ourCurrentNostrKey: state.ourCurrentNostrKey ? {
      publicKey: state.ourCurrentNostrKey.publicKey,
      privateKey: bytesToHex(state.ourCurrentNostrKey.privateKey),
    } : undefined,
    ourNextNostrKey: {
      publicKey: state.ourNextNostrKey.publicKey,
      privateKey: bytesToHex(state.ourNextNostrKey.privateKey),
    },
    receivingChainKey: state.receivingChainKey ? bytesToHex(state.receivingChainKey) : undefined,
    sendingChainKey: state.sendingChainKey ? bytesToHex(state.sendingChainKey) : undefined,
    sendingChainMessageNumber: state.sendingChainMessageNumber,
    receivingChainMessageNumber: state.receivingChainMessageNumber,
    previousSendingChainMessageCount: state.previousSendingChainMessageCount,
    skippedKeys: Object.fromEntries(
      Object.entries(state.skippedKeys).map(([pubkey, value]) => [
        pubkey,
        {
          headerKeys: value.headerKeys.map(bytes => bytesToHex(bytes)),
          messageKeys: Object.fromEntries(
            Object.entries(value.messageKeys).map(([index, bytes]) => [
              index,
              bytesToHex(bytes)
            ])
          )
        }
      ])
    )
  });
}

/**
 * Deserialize a channel state from a string.
 */
export function deserializeChannelState(data: string): ChannelState {
  const state = JSON.parse(data);
  return {
    rootKey: hexToBytes(state.rootKey),
    theirNostrPublicKey: state.theirNostrPublicKey,
    ourCurrentNostrKey: state.ourCurrentNostrKey ? {
      publicKey: state.ourCurrentNostrKey.publicKey,
      privateKey: hexToBytes(state.ourCurrentNostrKey.privateKey),
    } : undefined,
    ourNextNostrKey: {
      publicKey: state.ourNextNostrKey.publicKey,
      privateKey: hexToBytes(state.ourNextNostrKey.privateKey),
    },
    receivingChainKey: state.receivingChainKey ? hexToBytes(state.receivingChainKey) : undefined,
    sendingChainKey: state.sendingChainKey ? hexToBytes(state.sendingChainKey) : undefined,
    sendingChainMessageNumber: state.sendingChainMessageNumber,
    receivingChainMessageNumber: state.receivingChainMessageNumber,
    previousSendingChainMessageCount: state.previousSendingChainMessageCount,
    skippedKeys: Object.fromEntries(
      Object.entries(state.skippedKeys || {}).map(([pubkey, value]: [string, any]) => [
        pubkey,
        {
          headerKeys: value.headerKeys.map((hex: string) => hexToBytes(hex)),
          messageKeys: Object.fromEntries(
            Object.entries(value.messageKeys).map(([index, hex]) => [
              index,
              hexToBytes(hex as string)
            ])
          )
        }
      ])
    )
  };
}

/**
 * Create an async generator that yields messages as they arrive on a channel.
 */
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

export function kdf(input1: Uint8Array, input2: Uint8Array = new Uint8Array(32), numOutputs: number = 1): Uint8Array[] {
  const prk = hkdf_extract(sha256, input1, input2);
  
  const outputs: Uint8Array[] = [];
  for (let i = 1; i <= numOutputs; i++) {
    outputs.push(hkdf_expand(sha256, prk, new Uint8Array([i]), 32));
  }
  return outputs;
}
