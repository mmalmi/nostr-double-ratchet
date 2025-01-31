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

/** 
 * Represents the state of a Double Ratchet channel between two parties. Needed for persisting channels.
 */
export interface ChannelState {
  /** Root key used to derive new sending / receiving chain keys */
  rootKey: Uint8Array;
  
  /** The other party's current Nostr public key */
  theirCurrentNostrPublicKey: string;

  /** The other party's next Nostr public key (or current, if next not received yet) */
  theirNextNostrPublicKey: string;

  /** Our current Nostr keypair used for this channel */
  ourCurrentNostrKey: KeyPair;
  
  /** Our next Nostr keypair, used when ratcheting forward. It is advertised in messages we send. */
  ourNextNostrKey: KeyPair;
  
  /** Key for decrypting incoming messages in current chain */
  receivingChainKey: Uint8Array;
  
  /** Key for encrypting outgoing messages in current chain */
  sendingChainKey: Uint8Array;
  
  /** Number of messages sent in current sending chain */
  sendingChainMessageNumber: number;
  
  /** Number of messages received in current receiving chain */
  receivingChainMessageNumber: number;
  
  /** Number of messages sent in previous sending chain */
  previousSendingChainMessageCount: number;
  
  /** Cache of message keys for handling out-of-order messages */
  skippedMessageKeys: Record<string, Uint8Array>;
  
  /** Whether this party initiated the channel */
  isInitiator: boolean;
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