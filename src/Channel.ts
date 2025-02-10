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
import { kdf, skippedMessageIndexKey } from "./utils";

const MAX_SKIP = 1000;

/**
 * Similar to Signal's "Double Ratchet with header encryption"
 * https://signal.org/docs/specifications/doubleratchet/
 */
export class Channel {
  private nostrUnsubscribe?: Unsubscribe;
  private nostrNextUnsubscribe?: Unsubscribe;
  private internalSubscriptions = new Map<number, MessageCallback>();
  private currentInternalSubscriptionId = 0;
  public name: string;

  constructor(private nostrSubscribe: NostrSubscribe, public state: ChannelState) {
    this.name = Math.random().toString(36).substring(2, 6);
  }

  /**
   * @param sharedSecret optional, but useful to keep the first chain of messages secure. Unlike the Nostr keys, it can be forgotten after the 1st message in the chain.
   * @param isInitiator determines which chain key is used for sending vs receiving
   */
  static init(nostrSubscribe: NostrSubscribe, theirNostrPublicKey: string, ourCurrentPrivateKey: Uint8Array, sharedSecret = new Uint8Array(), name?: string, isInitiator = true): Channel {
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
      skippedMessageKeys: {},
    };
    const channel = new Channel(nostrSubscribe, state);
    if (name) channel.name = name;
    console.log(channel.name, 'initial root key', bytesToHex(state.rootKey).slice(0,4))
    return channel;
  }

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

  onMessage(callback: MessageCallback): Unsubscribe {
    const id = this.currentInternalSubscriptionId++
    this.internalSubscriptions.set(id, callback)
    this.subscribeToNostrEvents()
    return () => this.internalSubscriptions.delete(id)
  }

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
    const [intermediateRootKey, receivingChainKey] = kdf(this.state.rootKey, conversationKey1, 3);

    this.state.receivingChainKey = receivingChainKey;

    this.state.ourCurrentNostrKey = this.state.ourNextNostrKey;
    const ourNextSecretKey = generateSecretKey();
    this.state.ourNextNostrKey = {
      publicKey: getPublicKey(ourNextSecretKey),
      privateKey: ourNextSecretKey
    };

    const conversationKey2 = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, this.state.theirNostrPublicKey!);
    const [rootKey2, sendingChainKey] = kdf(intermediateRootKey, conversationKey2, 3);
    this.state.rootKey = rootKey2;
    this.state.sendingChainKey = sendingChainKey;
  }

  private skipMessageKeys(until: number, nostrSender: string) {
    if (this.state.receivingChainMessageNumber + MAX_SKIP < until) {
      throw new Error("Too many skipped messages");
    }
    while (this.state.receivingChainMessageNumber < until) {
      const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey!, new Uint8Array([1]), 2);
      this.state.receivingChainKey = newReceivingChainKey;
      const key = skippedMessageIndexKey(nostrSender, this.state.receivingChainMessageNumber);
      this.state.skippedMessageKeys[key] = messageKey;
      this.state.receivingChainMessageNumber++;
    }
  }

  private trySkippedMessageKeys(header: Header, ciphertext: string, nostrSender: string): string | null {
    const key = skippedMessageIndexKey(nostrSender, header.number);
    if (key in this.state.skippedMessageKeys) {
      const mk = this.state.skippedMessageKeys[key];
      delete this.state.skippedMessageKeys[key];
      return nip44.decrypt(ciphertext, mk);
    }
    return null;
  }

  private decryptHeader(e: any): [Header, boolean] {
    const encryptedHeader = e.tags[0][1];
    if (this.state.ourCurrentNostrKey) {
      const currentSecret = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, e.pubkey);
      try {
        const header = JSON.parse(nip44.decrypt(encryptedHeader, currentSecret)) as Header;
        return [header, false];
      } catch (error) {
        // Decryption with currentSecret failed, try with nextSecret
      }
    }

    const nextSecret = nip44.getConversationKey(this.state.ourNextNostrKey.privateKey, e.pubkey);
    try {
      const header = JSON.parse(nip44.decrypt(encryptedHeader, nextSecret)) as Header;
      return [header, true];
    } catch (error) {
      // Decryption with nextSecret also failed
    }

    throw new Error("Failed to decrypt header with both current and next secrets");
  }

  private handleNostrEvent(e: any) {
    const [header, shouldRatchet] = this.decryptHeader(e);

    if (this.state.theirNostrPublicKey !== header.nextPublicKey) {
      this.state.theirNostrPublicKey = header.nextPublicKey;
      this.nostrUnsubscribe?.(); // should we keep this open for a while? maybe as long as we have skipped messages?
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

    const data = this.ratchetDecrypt(header, e.content, e.pubkey);

    this.internalSubscriptions.forEach(callback => callback({id: e.id, data, pubkey: header.nextPublicKey, time: header.time}));  
  }

  private subscribeToNostrEvents() {
    if (this.nostrNextUnsubscribe) return;
    if (this.state.theirNostrPublicKey) {
      this.nostrUnsubscribe = this.nostrSubscribe(
        {authors: [this.state.theirNostrPublicKey], kinds: [EVENT_KIND]},
        (e) => this.handleNostrEvent(e)
      );
    }
    this.nostrNextUnsubscribe = this.nostrSubscribe(
      {authors: [this.state.theirNostrPublicKey], kinds: [EVENT_KIND]},
      (e) => this.handleNostrEvent(e)
    );
  }
}
