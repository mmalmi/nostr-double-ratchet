import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent, UnsignedEvent, getEventHash, validateEvent } from "nostr-tools";
import { bytesToHex } from "@noble/hashes/utils";
import {
  SessionState,
  Header,
  Unsubscribe,
  NostrSubscribe,
  EventCallback,
  MESSAGE_EVENT_KIND,
  Rumor,
  CHAT_MESSAGE_KIND,
} from "./types";
import { kdf, skippedMessageIndexKey } from "./utils";

const MAX_SKIP = 1000;

// 64 zeros
const DUMMY_PUBKEY = '0000000000000000000000000000000000000000000000000000000000000000'

/**
 * Double ratchet secure communication session over Nostr
 * 
 * Very similar to Signal's "Double Ratchet with header encryption"
 * https://signal.org/docs/specifications/doubleratchet/
 */
export class Session {
  private nostrUnsubscribe?: Unsubscribe;
  private nostrNextUnsubscribe?: Unsubscribe;
  private internalSubscriptions = new Map<number, EventCallback>();
  private currentInternalSubscriptionId = 0;
  public name: string;

  // 1. CHANNEL PUBLIC INTERFACE
  constructor(private nostrSubscribe: NostrSubscribe, public state: SessionState) {
    this.name = Math.random().toString(36).substring(2, 6);
  }

  /**
   * Initializes a new secure communication session
   * @param nostrSubscribe Function to subscribe to Nostr events
   * @param theirNextNostrPublicKey The public key of the other party
   * @param ourCurrentPrivateKey Our current private key for Nostr
   * @param isInitiator Whether we are initiating the conversation (true) or responding (false)
   * @param sharedSecret Initial shared secret for securing the first message chain
   * @param name Optional name for the session (for debugging)
   * @returns A new Session instance
   */
  static init(nostrSubscribe: NostrSubscribe, theirNextNostrPublicKey: string, ourCurrentPrivateKey: Uint8Array, isInitiator: boolean, sharedSecret: Uint8Array, name?: string): Session {
    const ourNextPrivateKey = generateSecretKey();
    const [rootKey, sendingChainKey] = kdf(sharedSecret, nip44.getConversationKey(ourNextPrivateKey, theirNextNostrPublicKey), 2);
    let ourCurrentNostrKey;
    let ourNextNostrKey;
    if (isInitiator) {
      ourCurrentNostrKey = { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey };
      ourNextNostrKey = { publicKey: getPublicKey(ourNextPrivateKey), privateKey: ourNextPrivateKey };
    } else {
      ourNextNostrKey = { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey };
    }
    const state: SessionState = {
      rootKey: isInitiator ? rootKey : sharedSecret,
      theirNextNostrPublicKey,
      ourCurrentNostrKey,
      ourNextNostrKey,
      receivingChainKey: undefined,
      sendingChainKey: isInitiator ? sendingChainKey : undefined,
      sendingChainMessageNumber: 0,
      receivingChainMessageNumber: 0,
      previousSendingChainMessageCount: 0,
      skippedMessageKeys: {},
      skippedHeaderKeys: {},
    };
    const session = new Session(nostrSubscribe, state);
    if (name) session.name = name;
    return session;
  }

  /**
   * Sends a text message through the encrypted session
   * @param text The plaintext message to send
   * @returns A verified Nostr event containing the encrypted message
   * @throws Error if we are not the initiator and trying to send the first message
   */
  send(text: string): {event: VerifiedEvent, innerEvent: Rumor} {
    return this.sendEvent({
      content: text,
      kind: CHAT_MESSAGE_KIND
    });
  }

  /**
   * Send a partial Nostr event through the encrypted session.
   * In addition to chat messages, it could be files, webrtc negotiation or many other types of messages.
   * @param event Partial Nostr event to send. Must be unsigned. Id and will be generated if not provided.
   * @returns A verified Nostr event containing the encrypted message
   * @throws Error if we are not the initiator and trying to send the first message
   */
  sendEvent(event: Partial<UnsignedEvent>): {event: VerifiedEvent, innerEvent: Rumor} {
    if (!this.state.theirNextNostrPublicKey || !this.state.ourCurrentNostrKey) {
      throw new Error("we are not the initiator, so we can't send the first message");
    }

    if ("sig" in event) {
      throw new Error("Event must be unsigned " + JSON.stringify(event));
    }

    const now = Date.now()

    const rumor: Partial<Rumor> = {
      ...event,
      content: event.content || "",
      kind: event.kind || MESSAGE_EVENT_KIND,
      created_at: event.created_at || Math.floor(now / 1000),
      tags: event.tags || [],
      pubkey: event.pubkey || DUMMY_PUBKEY,
    }

    if (!rumor.tags!.some(([k]) => k === "ms")) {
      rumor.tags!.push(["ms", String(now)])
    }

    rumor.id = getEventHash(rumor as Rumor);

    const [header, encryptedData] = this.ratchetEncrypt(JSON.stringify(rumor));
    
    const sharedSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, this.state.theirNextNostrPublicKey);
    const encryptedHeader = nip44.encrypt(JSON.stringify(header), sharedSecret);
    
    const nostrEvent = finalizeEvent({
      content: encryptedData,
      kind: MESSAGE_EVENT_KIND,
      tags: [["header", encryptedHeader]],
      created_at: Math.floor(Date.now() / 1000)
    }, this.state.ourCurrentNostrKey.privateKey);

    return {event: nostrEvent, innerEvent: rumor as Rumor};
  }

  /**
   * Subscribes to incoming messages on this session
   * @param callback Function to be called when a message is received
   * @returns Unsubscribe function to stop receiving messages
   */
  onEvent(callback: EventCallback): Unsubscribe {
    const id = this.currentInternalSubscriptionId++
    this.internalSubscriptions.set(id, callback)
    this.subscribeToNostrEvents()
    return () => this.internalSubscriptions.delete(id)
  }

  /**
   * Stop listening to incoming messages
   */
  close() {
    this.nostrUnsubscribe?.();
    this.nostrNextUnsubscribe?.();
  }

  // 2. RATCHET FUNCTIONS
  private ratchetEncrypt(plaintext: string): [Header, string] {
    const [newSendingChainKey, messageKey] = kdf(this.state.sendingChainKey!, new Uint8Array([1]), 2);
    this.state.sendingChainKey = newSendingChainKey;
    const header: Header = {
      number: this.state.sendingChainMessageNumber++,
      nextPublicKey: this.state.ourNextNostrKey.publicKey,
      previousChainLength: this.state.previousSendingChainMessageCount
    };
    return [header, nip44.encrypt(plaintext, messageKey)];
  }

  private ratchetDecrypt(header: Header, ciphertext: string, nostrSender: string): string {
    const plaintext = this.trySkippedMessageKeys(header, ciphertext, nostrSender);
    if (plaintext) return plaintext;

    this.skipMessageKeys(header.number, nostrSender);
    
    const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey!, new Uint8Array([1]), 2);
    this.state.receivingChainKey = newReceivingChainKey;
    this.state.receivingChainMessageNumber++;

    try {
      return nip44.decrypt(ciphertext, messageKey);
    } catch (error) {
      console.error(this.name, 'Decryption failed:', error, {
        messageKey: bytesToHex(messageKey).slice(0, 4),
        receivingChainKey: bytesToHex(this.state.receivingChainKey).slice(0, 4),
        sendingChainKey: this.state.sendingChainKey && bytesToHex(this.state.sendingChainKey).slice(0, 4),
        rootKey: bytesToHex(this.state.rootKey).slice(0, 4)
      });
      throw error;
    }
  }

  private ratchetStep(theirNextNostrPublicKey: string) {
    this.state.previousSendingChainMessageCount = this.state.sendingChainMessageNumber;
    this.state.sendingChainMessageNumber = 0;
    this.state.receivingChainMessageNumber = 0;
    this.state.theirNextNostrPublicKey = theirNextNostrPublicKey;

    const conversationKey1 = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, this.state.theirNextNostrPublicKey!);
    const [theirRootKey, receivingChainKey] = kdf(this.state.rootKey, conversationKey1, 2);

    this.state.receivingChainKey = receivingChainKey;

    this.state.ourCurrentNostrKey = this.state.ourNextNostrKey;
    const ourNextSecretKey = generateSecretKey();
    this.state.ourNextNostrKey = {
      publicKey: getPublicKey(ourNextSecretKey),
      privateKey: ourNextSecretKey
    };

    const conversationKey2 = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, this.state.theirNextNostrPublicKey!);
    const [rootKey, sendingChainKey] = kdf(theirRootKey, conversationKey2, 2);
    this.state.rootKey = rootKey;
    this.state.sendingChainKey = sendingChainKey;
  }

  // 3. MESSAGE KEY FUNCTIONS
  private skipMessageKeys(until: number, nostrSender: string) {
    if (this.state.receivingChainMessageNumber + MAX_SKIP < until) {
      throw new Error("Too many skipped messages");
    }
    while (this.state.receivingChainMessageNumber < until) {
      const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey!, new Uint8Array([1]), 2);
      this.state.receivingChainKey = newReceivingChainKey;
      const key = skippedMessageIndexKey(nostrSender, this.state.receivingChainMessageNumber);
      this.state.skippedMessageKeys[key] = messageKey;
      
      if (!this.state.skippedHeaderKeys[nostrSender]) {
        const secrets: Uint8Array[] = [];
        if (this.state.ourCurrentNostrKey) {
          const currentSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, nostrSender);
          secrets.push(currentSecret);
        }
        const nextSecret = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, nostrSender);
        secrets.push(nextSecret);
        this.state.skippedHeaderKeys[nostrSender] = secrets;
      }
      
      this.state.receivingChainMessageNumber++;
    }
  }

  private trySkippedMessageKeys(header: Header, ciphertext: string, nostrSender: string): string | null {
    const key = skippedMessageIndexKey(nostrSender, header.number);
    if (key in this.state.skippedMessageKeys) {
      const mk = this.state.skippedMessageKeys[key];
      delete this.state.skippedMessageKeys[key];
      
      // Check if we have any remaining skipped messages from this sender
      const hasMoreSkippedMessages = Object.keys(this.state.skippedMessageKeys).some(k => k.startsWith(`${nostrSender}:`));
      if (!hasMoreSkippedMessages) {
        // Clean up header keys and unsubscribe as no more skipped messages from this sender
        delete this.state.skippedHeaderKeys[nostrSender];
        this.nostrUnsubscribe?.();
        this.nostrUnsubscribe = undefined;
      }
      
      return nip44.decrypt(ciphertext, mk);
    }
    return null;
  }

  // 4. NOSTR EVENT HANDLING
  private decryptHeader(event: any): [Header, boolean, boolean] {
    const encryptedHeader = event.tags[0][1];
    if (this.state.ourCurrentNostrKey) {
      const currentSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, event.pubkey);
      try {
        const header = JSON.parse(nip44.decrypt(encryptedHeader, currentSecret)) as Header;
        return [header, false, false];
      } catch (error) {
        // Decryption with currentSecret failed, try with nextSecret
      }
    }

    const nextSecret = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, event.pubkey);
    try {
      const header = JSON.parse(nip44.decrypt(encryptedHeader, nextSecret)) as Header;
      return [header, true, false];
    } catch (error) {
      // Decryption with nextSecret also failed
    }

    const keys = this.state.skippedHeaderKeys[event.pubkey];
    if (keys) {
      for (const key of keys) {
        try {
          const header = JSON.parse(nip44.decrypt(encryptedHeader, key)) as Header;
          return [header, false, true];
        } catch (error) {
          // Decryption failed, try next secret
        }
      }
    }

    throw new Error("Failed to decrypt header with current and skipped header keys");
  }

  private handleNostrEvent(e: any) {
    const [header, shouldRatchet, isSkipped] = this.decryptHeader(e);

    if (!isSkipped) {
      if (this.state.theirNextNostrPublicKey !== header.nextPublicKey) {
        this.state.theirCurrentNostrPublicKey = this.state.theirNextNostrPublicKey;
        this.state.theirNextNostrPublicKey = header.nextPublicKey;
        this.nostrUnsubscribe?.(); // should we keep this open for a while? maybe as long as we have skipped messages?
        this.nostrUnsubscribe = this.nostrNextUnsubscribe;
        this.nostrNextUnsubscribe = this.nostrSubscribe(
          {authors: [this.state.theirNextNostrPublicKey], kinds: [MESSAGE_EVENT_KIND]},
          (e) => this.handleNostrEvent(e)
        );
      }
  
      if (shouldRatchet) {
        this.skipMessageKeys(header.previousChainLength, e.pubkey);
        this.ratchetStep(header.nextPublicKey);
      }
    } else {
      const key = skippedMessageIndexKey(e.pubkey, header.number);
      if (!(key in this.state.skippedMessageKeys)) {
        return // maybe we already processed this message
      }
    }

    const text = this.ratchetDecrypt(header, e.content, e.pubkey);
    const innerEvent = JSON.parse(text);
    if (!validateEvent(innerEvent)) {
      console.error("Invalid event received", innerEvent);
      return;
    }

    if (innerEvent.id !== getEventHash(innerEvent)) {
      console.error("Event hash does not match", innerEvent);
      return;
    }

    this.internalSubscriptions.forEach(callback => callback(innerEvent, e));  
  }

  private subscribeToNostrEvents() {
    if (this.nostrNextUnsubscribe) return;
    this.nostrNextUnsubscribe = this.nostrSubscribe(
      {authors: [this.state.theirNextNostrPublicKey], kinds: [MESSAGE_EVENT_KIND]},
      (e) => this.handleNostrEvent(e)
    );

    const authors = Object.keys(this.state.skippedHeaderKeys);
    if (this.state.theirCurrentNostrPublicKey && !authors.includes(this.state.theirCurrentNostrPublicKey)) {
      authors.push(this.state.theirCurrentNostrPublicKey)
    }
    // do we want this unsubscribed on rotation or should we keep it open
    // in case more skipped messages are found by relays or peers?
    this.nostrUnsubscribe = this.nostrSubscribe(
      {authors, kinds: [MESSAGE_EVENT_KIND]},
      (e) => this.handleNostrEvent(e)
    );
  }
}
