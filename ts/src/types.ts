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
export type NostrSubscribe = (_filter: Filter, _onEvent: (_e: VerifiedEvent) => void) => Unsubscribe;
export type EncryptFunction = (_plaintext: string, _pubkey: string) => Promise<string>;
export type DecryptFunction = (_ciphertext: string, _pubkey: string) => Promise<string>;

/**
 * Identity key for cryptographic operations.
 * Either a raw private key (Uint8Array) or encrypt/decrypt functions for extension login (NIP-07).
 */
export type IdentityKey = Uint8Array | { encrypt: EncryptFunction; decrypt: DecryptFunction };

export type NostrPublish = (_event: UnsignedEvent) => Promise<VerifiedEvent>;

export type Rumor = UnsignedEvent & { id: string }

/**
 * Callback function for handling decrypted messages
 * @param _event - The decrypted message object (Rumor)
 * @param _outerEvent - The outer Nostr event (VerifiedEvent)
 */
export type EventCallback = (_event: Rumor, _outerEvent: VerifiedEvent) => void;

/**
 * Message event kind
 */
export const MESSAGE_EVENT_KIND = 1060;

/**
 * Invite event kind
 */
export const INVITE_EVENT_KIND = 30078;

export const INVITE_RESPONSE_KIND = 1059;

/**
 * Invite list event kind (replaceable - one per user)
 */
export const INVITE_LIST_EVENT_KIND = 10078;

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

/**
 * Payload for reaction messages sent through NDR.
 * Reactions are regular messages with a JSON payload indicating they're a reaction.
 */
export interface ReactionPayload {
  type: 'reaction';
  /** ID of the message being reacted to */
  messageId: string;
  /** Emoji or reaction content */
  emoji: string;
}

/**
 * Kind constant for reaction inner events
 */
export const REACTION_KIND = 7;
