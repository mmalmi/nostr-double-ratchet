import { hexToBytes, bytesToHex } from "@noble/hashes/utils";
import { SessionState, Message } from "./types";
import { Session } from "./Session.ts";
import { extract as hkdf_extract, expand as hkdf_expand } from '@noble/hashes/hkdf';
import { sha256 } from '@noble/hashes/sha256';

export function serializeSessionState(state: SessionState): string {
  return JSON.stringify({
    rootKey: bytesToHex(state.rootKey),
    theirCurrentNostrPublicKey: state.theirCurrentNostrPublicKey,
    theirNextNostrPublicKey: state.theirNextNostrPublicKey,
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
    skippedMessageKeys: Object.fromEntries(
      Object.entries(state.skippedMessageKeys).map(([key, value]) => [
        key,
        bytesToHex(value),
      ])
    ),
    skippedHeaderKeys: Object.fromEntries(
      Object.entries(state.skippedHeaderKeys).map(([key, value]) => [
        key,
        value.map(bytes => bytesToHex(bytes))
      ])
    ),
  });
}

export function deserializeSessionState(data: string): SessionState {
  const state = JSON.parse(data);
  return {
    rootKey: hexToBytes(state.rootKey),
    theirCurrentNostrPublicKey: state.theirCurrentNostrPublicKey,
    theirNextNostrPublicKey: state.theirNextNostrPublicKey,
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
    skippedMessageKeys: Object.fromEntries(
      Object.entries(state.skippedMessageKeys).map(([key, value]) => [
        key,
        hexToBytes(value as string),
      ])
    ),
    skippedHeaderKeys: Object.fromEntries(
      Object.entries(state.skippedHeaderKeys || {}).map(([key, value]) => [
        key,
        (value as string[]).map(hex => hexToBytes(hex))
      ])
    ),
  };
}

export async function* createMessageStream(session: Session): AsyncGenerator<Message, void, unknown> {
  const messageQueue: Message[] = [];
  let resolveNext: ((value: Message) => void) | null = null;

  const unsubscribe = session.onMessage((message) => {
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

export function skippedMessageIndexKey(nostrSender: string, number: number): string {
  return `${nostrSender}:${number}`;
}