import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent } from "nostr-tools";
import { bytesToHex } from "@noble/hashes/utils";
import {
  ChannelState,
  Header,
  Unsubscribe,
  NostrSubscribe,
  MessageCallback,
  EVENT_KIND,
} from "./types";
import { kdf } from "./utils";

const MAX_SKIP = 1000;

/**
 * Double ratchet secure communication channel over Nostr
 * 
 * Very similar to Signal's "Double Ratchet with header encryption"
 * https://signal.org/docs/specifications/doubleratchet/
 */
export class Channel {
  private nostrUnsubscribe?: Unsubscribe;
  private nostrNextUnsubscribe?: Unsubscribe;
  private internalSubscriptions = new Map<number, MessageCallback>();
  private currentInternalSubscriptionId = 0;
  public name: string;

  // 1. CHANNEL PUBLIC INTERFACE
  /**
   * Creates a new Channel instance
   * @param nostrSubscribe Function to subscribe to Nostr events
   * @param state Saved state of the channel. Get from deserializeChannelState, or use Channel.init instead to create a new channel.
   */
  constructor(private nostrSubscribe: NostrSubscribe, public state: ChannelState) {
    this.name = Math.random().toString(36).substring(2, 6);
  }

  /**
   * Initializes a new secure communication channel
   * @param nostrSubscribe Function to subscribe to Nostr events
   * @param theirNostrPublicKey The public key of the other party
   * @param ourCurrentPrivateKey Our current private key for Nostr
   * @param isInitiator Whether we are initiating the conversation (true) or responding (false)
   * @param sharedSecret Initial shared secret for securing the first message chain
   * @param name Optional name for the channel (for debugging)
   * @returns A new Channel instance
   */
  static init(nostrSubscribe: NostrSubscribe, theirNostrPublicKey: string, ourCurrentPrivateKey: Uint8Array, isInitiator: boolean, sharedSecret: Uint8Array, name?: string): Channel {
    const ourNextPrivateKey = generateSecretKey();
    const [rootKey, sendingChainKey] = kdf(sharedSecret, nip44.getConversationKey(ourNextPrivateKey, theirNostrPublicKey), 2);
    let ourCurrentNostrKey;
    let ourNextNostrKey;
    if (isInitiator) {
      ourCurrentNostrKey = { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey };
      ourNextNostrKey = { publicKey: getPublicKey(ourNextPrivateKey), privateKey: ourNextPrivateKey };
    } else {
      ourNextNostrKey = { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey };
    }
    const state: ChannelState = {
      rootKey: isInitiator ? rootKey : sharedSecret,
      theirNostrPublicKey,
      ourCurrentNostrKey,
      ourNextNostrKey,
      receivingChainKey: undefined,
      sendingChainKey: isInitiator ? sendingChainKey : undefined,
      sendingChainMessageNumber: 0,
      receivingChainMessageNumber: 0,
      previousSendingChainMessageCount: 0,
      skippedKeys: {},
    };
    const channel = new Channel(nostrSubscribe, state);
    if (name) channel.name = name;
    return channel;
  }

  /**
   * Sends an encrypted message through the channel
   * @param data The plaintext message to send
   * @returns A verified Nostr event containing the encrypted message
   * @throws Error if we are not the initiator and trying to send the first message
   */
  send(data: string): VerifiedEvent {
    if (!this.state.theirNostrPublicKey || !this.state.ourCurrentNostrKey) {
      throw new Error("we are not the initiator, so we can't send the first message");
    }

    const [header, encryptedData] = this.ratchetEncrypt(data);
    
    const sharedSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, this.state.theirNostrPublicKey);
    const encryptedHeader = nip44.encrypt(JSON.stringify(header), sharedSecret);
    
    const nostrEvent = finalizeEvent({
      content: encryptedData,
      kind: EVENT_KIND,
      tags: [["header", encryptedHeader]],
      created_at: Math.floor(Date.now() / 1000)
    }, this.state.ourCurrentNostrKey.privateKey);

    return nostrEvent;
  }

  /**
   * Subscribes to incoming messages on this channel
   * @param callback Function to be called when a message is received
   * @returns Unsubscribe function to stop receiving messages
   */
  onMessage(callback: MessageCallback): Unsubscribe {
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
      time: Date.now(),
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

  private ratchetStep(theirNostrPublicKey: string) {
    this.state.previousSendingChainMessageCount = this.state.sendingChainMessageNumber;
    this.state.sendingChainMessageNumber = 0;
    this.state.receivingChainMessageNumber = 0;
    this.state.theirNostrPublicKey = theirNostrPublicKey;

    const conversationKey1 = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, this.state.theirNostrPublicKey!);
    const [theirRootKey, receivingChainKey] = kdf(this.state.rootKey, conversationKey1, 2);

    this.state.receivingChainKey = receivingChainKey;

    this.state.ourCurrentNostrKey = this.state.ourNextNostrKey;
    const ourNextSecretKey = generateSecretKey();
    this.state.ourNextNostrKey = {
      publicKey: getPublicKey(ourNextSecretKey),
      privateKey: ourNextSecretKey
    };

    const conversationKey2 = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, this.state.theirNostrPublicKey!);
    const [rootKey, sendingChainKey] = kdf(theirRootKey, conversationKey2, 2);
    this.state.rootKey = rootKey;
    this.state.sendingChainKey = sendingChainKey;
  }

  // 3. MESSAGE KEY FUNCTIONS
  private skipMessageKeys(until: number, nostrSender: string) {
    if (this.state.receivingChainMessageNumber + MAX_SKIP < until) {
      throw new Error("Too many skipped messages");
    }

    if (!this.state.skippedKeys[nostrSender]) {
      this.state.skippedKeys[nostrSender] = {
        headerKeys: [],
        messageKeys: {}
      };
      
      // Store header keys
      if (this.state.ourCurrentNostrKey) {
        const currentSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, nostrSender);
        this.state.skippedKeys[nostrSender].headerKeys.push(currentSecret);
      }
      const nextSecret = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, nostrSender);
      this.state.skippedKeys[nostrSender].headerKeys.push(nextSecret);
    }

    while (this.state.receivingChainMessageNumber < until) {
      const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey!, new Uint8Array([1]), 2);
      this.state.receivingChainKey = newReceivingChainKey;
      this.state.skippedKeys[nostrSender].messageKeys[this.state.receivingChainMessageNumber] = messageKey;
      this.state.receivingChainMessageNumber++;
    }
  }

  private trySkippedMessageKeys(header: Header, ciphertext: string, nostrSender: string): string | null {
    const skippedKeys = this.state.skippedKeys[nostrSender];
    if (!skippedKeys) return null;

    const messageKey = skippedKeys.messageKeys[header.number];
    if (!messageKey) return null;

    delete skippedKeys.messageKeys[header.number];
    
    // Clean up if no more skipped messages
    if (Object.keys(skippedKeys.messageKeys).length === 0) {
      delete this.state.skippedKeys[nostrSender];
      this.nostrUnsubscribe?.();
      this.nostrUnsubscribe = undefined;
    }
    
    return nip44.decrypt(ciphertext, messageKey);
  }

  // 4. NOSTR EVENT HANDLING
  private decryptHeader(event: any): [Header, boolean, boolean] {
    const encryptedHeader = event.tags[0][1];
    
    // Try current key
    if (this.state.ourCurrentNostrKey) {
      const currentSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, event.pubkey);
      try {
        const header = JSON.parse(nip44.decrypt(encryptedHeader, currentSecret)) as Header;
        return [header, false, false];
      } catch (error) {}
    }

    // Try next key
    const nextSecret = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, event.pubkey);
    try {
      const header = JSON.parse(nip44.decrypt(encryptedHeader, nextSecret)) as Header;
      return [header, true, false];
    } catch (error) {}

    // Try skipped keys
    const skippedData = this.state.skippedKeys[event.pubkey];
    if (skippedData) {
      for (const key of skippedData.headerKeys) {
        try {
          const header = JSON.parse(nip44.decrypt(encryptedHeader, key)) as Header;
          return [header, false, true];
        } catch (error) {}
      }
    }

    throw new Error("Failed to decrypt header with current and skipped header keys");
  }

  private handleNostrEvent(e: any) {
    const [header, shouldRatchet, isSkipped] = this.decryptHeader(e);

    if (!isSkipped) {
      if (this.state.theirNostrPublicKey !== header.nextPublicKey) {
        this.state.theirNostrPublicKey = header.nextPublicKey;
        this.nostrUnsubscribe?.();
        this.nostrUnsubscribe = this.nostrNextUnsubscribe;
        this.nostrNextUnsubscribe = this.nostrSubscribe(
          {authors: [this.state.theirNostrPublicKey], kinds: [EVENT_KIND]},
          (e) => this.handleNostrEvent(e)
        );
      }
  
      if (shouldRatchet) {
        this.skipMessageKeys(header.previousChainLength, e.pubkey);
        this.ratchetStep(header.nextPublicKey);
      }
    } else {
      if (!(header.number in this.state.skippedKeys[e.pubkey].messageKeys)) {
        return // maybe we already processed this message
      }
    }

    const data = this.ratchetDecrypt(header, e.content, e.pubkey);

    this.internalSubscriptions.forEach(callback => callback({id: e.id, data, pubkey: header.nextPublicKey, time: header.time}));  
  }

  private subscribeToNostrEvents() {
    if (this.nostrNextUnsubscribe) return;
    this.nostrNextUnsubscribe = this.nostrSubscribe(
      {authors: [this.state.theirNostrPublicKey], kinds: [EVENT_KIND]},
      (e) => this.handleNostrEvent(e)
    );

    const skippedSenders = Object.keys(this.state.skippedKeys);
    if (skippedSenders.length > 0) {
      // do we want this unsubscribed on rotation or should we keep it open
      // in case more skipped messages are found by relays or peers?
      this.nostrUnsubscribe = this.nostrSubscribe(
        {authors: skippedSenders, kinds: [EVENT_KIND]},
        (e) => this.handleNostrEvent(e)
      );
    }
  }
}
