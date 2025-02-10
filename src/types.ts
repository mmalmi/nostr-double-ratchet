import { VerifiedEvent } from "nostr-tools";

/**
 * An event that has been verified to be from the Nostr network.
 */
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

/**
 * A keypair used for encryption and decryption.
 */
export type KeyPair = {
  publicKey: string;
  privateKey: Uint8Array;
}

/** 
 * State of a Double Ratchet channel between two parties. Needed for persisting channels.
 */
export interface ChannelState {
  /** Root key used to derive new sending / receiving chain keys */
  rootKey: Uint8Array;
  
  /** The other party's current Nostr public key */
  theirNostrPublicKey: string;

  /** Our current Nostr keypair used for this channel */
  ourCurrentNostrKey?: KeyPair;
  
  /** Our next Nostr keypair, used when ratcheting forward. It is advertised in messages we send. */
  ourNextNostrKey: KeyPair;
  
  /** Key for decrypting incoming messages in current chain */
  receivingChainKey?: Uint8Array;
  
  /** Key for encrypting outgoing messages in current chain */
  sendingChainKey?: Uint8Array;
  
  /** Number of messages sent in current sending chain */
  sendingChainMessageNumber: number;
  
  /** Number of messages received in current receiving chain */
  receivingChainMessageNumber: number;
  
  /** Number of messages sent in previous sending chain */
  previousSendingChainMessageCount: number;
  
  /** Cache of message keys for handling out-of-order messages */
  skippedMessageKeys: Record<string, Uint8Array>;
}

/** Function to unsubscribe from a Nostr subscription */
export type Unsubscribe = () => void;

/** 
 * Function that subscribes to Nostr events matching a filter and calls onEvent for each event.
 */
export type NostrSubscribe = (filter: NostrFilter, onEvent: (e: VerifiedEvent) => void) => Unsubscribe;

/** 
 * Callback function for handling decrypted messages
 * @param message - The decrypted message object
 */
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

export type EncryptFunction = (plaintext: string, pubkey: string) => Promise<string>;
export type DecryptFunction = (ciphertext: string, pubkey: string) => Promise<string>;