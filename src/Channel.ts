import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent } from "nostr-tools";
import { hexToBytes, bytesToHex } from "@noble/hashes/utils";
import {
  ChannelState,
  Header,
  Unsubscribe,
  NostrSubscribe,
  MessageCallback,
  EVENT_KIND,
  KeyPair,
  Sender,
} from "./types";
import { kdf, skippedMessageIndexKey } from "./utils";

const MAX_SKIP = 1000;

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
  static init(nostrSubscribe: NostrSubscribe, theirCurrentNostrPublicKey: string, ourCurrentPrivateKey: Uint8Array, sharedSecret = new Uint8Array(), name?: string, isInitiator = true): Channel {
    const ourNextPrivateKey = generateSecretKey();
    const state: ChannelState = {
      rootKey: sharedSecret,
      theirCurrentNostrPublicKey,
      theirNextNostrPublicKey: theirCurrentNostrPublicKey,
      ourCurrentNostrKey: { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey },
      ourNextNostrKey: { publicKey: getPublicKey(ourNextPrivateKey), privateKey: ourNextPrivateKey },
      receivingChainKey: new Uint8Array(),
      sendingChainKey: new Uint8Array(),
      sendingChainMessageNumber: 0,
      receivingChainMessageNumber: 0,
      previousSendingChainMessageCount: 0,
      skippedMessageKeys: {},
      isInitiator,
    };
    const channel = new Channel(nostrSubscribe, state);
    if (name) channel.name = name;
    return channel;
  }

  send(data: string): VerifiedEvent {
    const [header, encryptedData] = this.ratchetEncrypt(data);
    
    const sendingNostrPrivateKey = this.getNostrSenderKeypair(Sender.Us, this.state.ourCurrentNostrKey.privateKey, this.state.theirNextNostrPublicKey).privateKey;
    const encryptedHeader = nip44.encrypt(JSON.stringify(header), sendingNostrPrivateKey);
    
    const nostrEvent = finalizeEvent({
      content: encryptedData,
      kind: EVENT_KIND,
      tags: [["header", encryptedHeader]],
      created_at: Math.floor(Date.now() / 1000)
    }, sendingNostrPrivateKey);

    return nostrEvent;
  }

  onMessage(callback: MessageCallback): Unsubscribe {
    const id = this.currentInternalSubscriptionId++
    this.internalSubscriptions.set(id, callback)
    this.subscribeToNostrEvents()
    return () => this.internalSubscriptions.delete(id)
  }

  getNostrSenderKeypair(sender: Sender, ourPrivate: Uint8Array, theirPublic: string): KeyPair {
    const senderPubKey = sender === Sender.Us ? getPublicKey(ourPrivate) : theirPublic
    const [privateKey] = kdf(nip44.getConversationKey(ourPrivate, theirPublic), hexToBytes(senderPubKey))
    return {
      publicKey: getPublicKey(privateKey),
      privateKey
    }
  }

  private ratchetEncrypt(plaintext: string): [Header, string] {
    const [newSendingChainKey, messageKey] = kdf(this.state.sendingChainKey, new Uint8Array([1]), 2);
    console.log(this.name, 'ratchetEncrypt', plaintext.slice(0,10), 'newSendingChainKey', bytesToHex(newSendingChainKey).slice(0, 4), 'old sendingChainKey', bytesToHex(this.state.sendingChainKey).slice(0, 4));
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

    this.skipMessageKeys(header.number);
    
    const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey, new Uint8Array([1]), 2);
    console.log(this.name, 'ratchetDecrypt', 'newReceivingChainKey', bytesToHex(newReceivingChainKey).slice(0, 4), 'old receivingChainKey', bytesToHex(this.state.receivingChainKey).slice(0, 4));
    this.state.receivingChainKey = newReceivingChainKey;
    this.state.receivingChainMessageNumber++;

    if (header.nextPublicKey !== this.state.theirNextNostrPublicKey) {
      // TODO may be problem here if old message received
      this.skipMessageKeys(header.previousChainLength);
      this.state.theirCurrentNostrPublicKey = this.state.theirNextNostrPublicKey;
      this.state.theirNextNostrPublicKey = header.nextPublicKey;
    }

    try {
      return nip44.decrypt(ciphertext, messageKey);
    } catch (error) {
      console.log(this.name, 'Decryption failed:', error, {
        messageKey: bytesToHex(messageKey).slice(0, 4),
        receivingChainKey: bytesToHex(this.state.receivingChainKey).slice(0, 4),
        sendingChainKey: bytesToHex(this.state.sendingChainKey).slice(0, 4),
        rootKey: bytesToHex(this.state.rootKey).slice(0, 4)
      });
      throw error;
    }
  }

  private updateRootKey() {
    this.state.previousSendingChainMessageCount = this.state.sendingChainMessageNumber;
    this.state.sendingChainMessageNumber = 0;
    this.state.receivingChainMessageNumber = 0;
    const conversationKey = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, this.state.theirCurrentNostrPublicKey);
    const [rootKey, chainKey1, chainKey2] = kdf(this.state.rootKey, conversationKey, 3);
    console.log(this.name, 'updateRootKey', 'old rootKey', bytesToHex(this.state.rootKey).slice(0, 4), 'new rootKey', bytesToHex(rootKey).slice(0, 4))
    this.state.rootKey = rootKey;
    this.state.receivingChainKey.length && console.log(this.name, 'updateRootKey', 'old receivingChainKey', bytesToHex(this.state.receivingChainKey).slice(0, 4), 'old sendingChainKey', bytesToHex(this.state.sendingChainKey).slice(0, 4))
    this.state.receivingChainKey = this.state.isInitiator ? chainKey2 : chainKey1;
    this.state.sendingChainKey = this.state.isInitiator ? chainKey1 : chainKey2;
    console.log(this.name, 'updateRootKey', 'new receivingChainKey', bytesToHex(this.state.receivingChainKey).slice(0, 4), 'new sendingChainKey', bytesToHex(this.state.sendingChainKey).slice(0, 4))

    //this.nostrUnsubscribe?.();
    this.nostrUnsubscribe = this.nostrNextUnsubscribe;
    this.subscribeToNextNostrEvents();
  }

  private updateOurNostrKeys() {
    console.log(this.name, 'updateOurNostrKeys')
    this.state.ourCurrentNostrKey = this.state.ourNextNostrKey;
    const ourNextSecretKey = generateSecretKey();
    this.state.ourNextNostrKey = {
      publicKey: getPublicKey(ourNextSecretKey),
      privateKey: ourNextSecretKey
    };
    this.updateRootKey();
  }

  private skipMessageKeys(until: number) {
    if (this.state.receivingChainMessageNumber + MAX_SKIP < until) {
      throw new Error("Too many skipped messages");
    }
    const nostrSender = this.getNostrSenderKeypair(Sender.Them, this.state.ourCurrentNostrKey.privateKey, this.state.theirCurrentNostrPublicKey).publicKey;
    while (this.state.receivingChainMessageNumber < until) {
      console.log('skipping message key', this.state.receivingChainMessageNumber)
      const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey, new Uint8Array([1]), 2);
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

  // Nostr event handling methods
  private handleNostrEvent(e: any, receivingNostrKey: KeyPair) {
    const header = JSON.parse(nip44.decrypt(e.tags[0][1], receivingNostrKey.privateKey)) as Header;
    const isNextKey = receivingNostrKey.publicKey === this.getNostrSenderKeypair(Sender.Them, this.state.ourNextNostrKey.privateKey, this.state.theirCurrentNostrPublicKey).publicKey;
    if (isNextKey) {
      // they acknowledged our next key, so we can rotate it again
      this.nostrUnsubscribe?.();
      this.updateOurNostrKeys();
    }
    const data = this.ratchetDecrypt(header, e.content, e.pubkey);
    this.internalSubscriptions.forEach(callback => callback({id: e.id, data, pubkey: header.nextPublicKey, time: header.time}));  
  }

  private subscribeToNostrEvents() {
    if (this.nostrUnsubscribe) return;
    
    const receivingNostrKey = this.getNostrSenderKeypair(Sender.Them, this.state.ourCurrentNostrKey.privateKey, this.state.theirCurrentNostrPublicKey);
    this.nostrUnsubscribe = this.nostrSubscribe(
      {authors: [receivingNostrKey.publicKey], kinds: [EVENT_KIND]},
      (e) => this.handleNostrEvent(e, receivingNostrKey)
    );
    
    this.subscribeToNextNostrEvents();
  }

  private subscribeToNextNostrEvents() {
    const nextReceivingNostrKey = this.getNostrSenderKeypair(Sender.Them, this.state.ourNextNostrKey.privateKey, this.state.theirCurrentNostrPublicKey);
    this.nostrNextUnsubscribe = this.nostrSubscribe(
      {authors: [nextReceivingNostrKey.publicKey], kinds: [EVENT_KIND]},
      (e) => this.handleNostrEvent(e, nextReceivingNostrKey)
    );
  }
}
