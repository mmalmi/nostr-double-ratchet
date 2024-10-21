import { VerifiedEvent } from "nostr-tools";

export type Message = {
  id: string;
  data: string;
  pubkey: string;
  time: number; // unlike Nostr, we use milliseconds instead of seconds
}

export type Header = {
  number: number;
  previousChainLength: number;
  nextPublicKey: string;
  time: number;
}

export type NostrFilter = {
  authors?: string[];
  kinds?: number[];
}

export type KeyPair = {
  publicKey: string;
  privateKey: Uint8Array;
}

export interface ChannelState {
  rootKey: Uint8Array;
  theirCurrentNostrPublicKey: string;
  ourCurrentNostrKey: KeyPair;
  ourNextNostrKey: KeyPair;
  receivingChainKey: Uint8Array;
  sendingChainKey: Uint8Array;
  sendingChainMessageNumber: number;
  receivingChainMessageNumber: number;
  previousSendingChainMessageCount: number;
  skippedMessageKeys: Record<string, Uint8Array>;
}

export type Unsubscribe = () => void;
export type NostrSubscribe = (filter: NostrFilter, onEvent: (e: VerifiedEvent) => void) => Unsubscribe;
export type MessageCallback = (message: Message) => void;

export const EVENT_KIND = 4;
export const MAX_SKIP = 100;

export type NostrEvent = {
  id: string;
  pubkey: string;
  created_at: number;
  kind: number;
  tags: string[][];
  content: string;
  sig: string;
}

export enum Sender {
  Us,
  Them
}

export enum KeyType {
  Current,
  Next
}

export type EncryptFunction = (plaintext: string, pubkey: string) => Promise<string>;
export type DecryptFunction = (ciphertext: string, pubkey: string) => Promise<string>;