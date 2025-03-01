import { Filter, UnsignedEvent, VerifiedEvent } from "nostr-tools";

export type Header = {
  number: number;
  previousChainLength: number;
  nextPublicKey: string;
}

/**
 * A keypair used for encryption and decryption.
 */
export type KeyPair = {
  publicKey: string;
  privateKey: Uint8Array;
}

/** 
 * State of a Double Ratchet session between two parties. Needed for persisting sessions.
 */
export interface SessionState {
  /** Root key used to derive new sending / receiving chain keys */
  rootKey: Uint8Array;
  
  /** The other party's current Nostr public key */
  theirCurrentNostrPublicKey?: string;

  /** The other party's next Nostr public key */
  theirNextNostrPublicKey: string;

  /** Our current Nostr keypair used for this session */
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
  
  /** Cache of message & header keys for handling out-of-order messages */
  skippedKeys: {
    [pubKey: string]: {
      headerKeys: Uint8Array[],
      messageKeys: {[msgIndex: number]: Uint8Array}
    };
  };
}

/**
 * Unsubscribe from a subscription or event listener.
 */
export type Unsubscribe = () => void;

/** 
 * Function that subscribes to Nostr events matching a filter and calls onEvent for each event.
 */
export type NostrSubscribe = (filter: Filter, onEvent: (e: VerifiedEvent) => void) => Unsubscribe;
export type EncryptFunction = (plaintext: string, pubkey: string) => Promise<string>;
export type DecryptFunction = (ciphertext: string, pubkey: string) => Promise<string>;
export type NostrPublish = (event: UnsignedEvent) => Promise<VerifiedEvent>;

export type Rumor = UnsignedEvent & { id: string }

/** 
 * Callback function for handling decrypted messages
 * @param message - The decrypted message object
 */
export type EventCallback = (event: Rumor, outerEvent: VerifiedEvent) => void;

/**
 * Message event kind
 */
export const MESSAGE_EVENT_KIND = 1060;

/**
 * Invite event kind
 */
export const INVITE_EVENT_KIND = 30078;

export const INVITE_RESPONSE_KIND = 1059;

export const CHAT_MESSAGE_KIND = 14;

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
