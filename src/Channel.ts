import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent } from "nostr-tools";
import { hexToBytes } from "@noble/hashes/utils";
import {
  ChannelState,
  Header,
  Unsubscribe,
  NostrSubscribe,
  MessageCallback,
  EVENT_KIND,
  KeyPair,
  Sender,
  KeyType,
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
   */
  static init(nostrSubscribe: NostrSubscribe, theirCurrentNostrPublicKey: string, ourCurrentPrivateKey: Uint8Array, sharedSecret = new Uint8Array(), name?: string): Channel {
    const ourNextPrivateKey = generateSecretKey();
    const state: ChannelState = {
      rootKey: sharedSecret,
      theirCurrentNostrPublicKey,
      ourCurrentNostrKey: { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey },
      ourNextNostrKey: { publicKey: getPublicKey(ourNextPrivateKey), privateKey: ourNextPrivateKey },
      receivingChainKey: new Uint8Array(),
      sendingChainKey: new Uint8Array(),
      sendingChainMessageNumber: 0,
      receivingChainMessageNumber: 0,
      previousSendingChainMessageCount: 0,
      skippedMessageKeys: {},
    };
    const channel = new Channel(nostrSubscribe, state);
    channel.updateTheirCurrentNostrPublicKey(theirCurrentNostrPublicKey);
    if (name) channel.name = name;
    return channel;
  }

  send(data: string): VerifiedEvent {
    const [header, encryptedData] = this.ratchetEncrypt(data);
    
    const sendingNostrPrivateKey = this.getNostrSenderKeypair(Sender.Us, KeyType.Current).privateKey;
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

  getNostrSenderKeypair(sender: Sender, keyType: KeyType): KeyPair {
    if (sender === Sender.Us && keyType === KeyType.Next) {
      throw new Error("We don't have their next key")
    }
    const ourPrivate = keyType === KeyType.Current ? this.state.ourCurrentNostrKey.privateKey : this.state.ourNextNostrKey.privateKey
    const theirPublic = this.state.theirCurrentNostrPublicKey
    const senderPubKey = sender === Sender.Us ? getPublicKey(ourPrivate) : theirPublic
    const [privateKey] = kdf(nip44.getConversationKey(ourPrivate, theirPublic), hexToBytes(senderPubKey))
    return {
      publicKey: getPublicKey(privateKey),
      privateKey
    }
  }

  private ratchetEncrypt(plaintext: string): [Header, string] {
    const [newSendingChainKey, messageKey] = kdf(this.state.sendingChainKey, new Uint8Array([1]), 2);
    this.state.sendingChainKey = newSendingChainKey;
    const header: Header = {
      number: this.state.sendingChainMessageNumber++,
      nextPublicKey: this.state.ourNextNostrKey.publicKey,
      time: Date.now(),
      previousChainLength: this.state.previousSendingChainMessageCount
    };
    return [header, nip44.encrypt(plaintext, messageKey)];
  }

  private ratchetDecrypt(header: Header, ciphertext: string, nostrSender: string, first = false): string {
    const plaintext = this.trySkippedMessageKeys(header, ciphertext, nostrSender);
    if (plaintext) return plaintext;

    this.skipMessageKeys(header.number);

    if (header.nextPublicKey !== this.state.theirCurrentNostrPublicKey) {
      this.skipMessageKeys(header.previousChainLength);
      if (!first) {
        this.rotateOurCurrentNostrKey();
        this.updateTheirCurrentNostrPublicKey(header.nextPublicKey);
      }
    }
    
    const [newReceivingChainKey, messageKey] = kdf(this.state.receivingChainKey, new Uint8Array([1]), 2);
    this.state.receivingChainKey = newReceivingChainKey;
    this.state.receivingChainMessageNumber++;

    return nip44.decrypt(ciphertext, messageKey);
  }

  private updateTheirCurrentNostrPublicKey(theirNewPublicKey: string) {
    this.state.theirCurrentNostrPublicKey = theirNewPublicKey;
    this.state.previousSendingChainMessageCount = this.state.sendingChainMessageNumber;
    this.state.sendingChainMessageNumber = 0;
    this.state.receivingChainMessageNumber = 0;
    const conversationKey = nip44.getConversationKey(this.state.ourCurrentNostrKey.privateKey, theirNewPublicKey);
    const [rootKey, chainKey1, chainKey2] = kdf(this.state.rootKey, conversationKey, 3);
    this.state.rootKey = rootKey;
    const isOurKeyGreater = this.state.ourCurrentNostrKey.publicKey > theirNewPublicKey;
    this.state.receivingChainKey = isOurKeyGreater ? chainKey1 : chainKey2;
    this.state.sendingChainKey = isOurKeyGreater ? chainKey2 : chainKey1;
  }

  private rotateOurCurrentNostrKey() {
    this.state.ourCurrentNostrKey = this.state.ourNextNostrKey;
    const ourNextSecretKey = generateSecretKey();
    this.state.ourNextNostrKey = {
      publicKey: getPublicKey(ourNextSecretKey),
      privateKey: ourNextSecretKey
    };
  }

  private skipMessageKeys(until: number) {
    if (this.state.receivingChainMessageNumber + MAX_SKIP < until) {
      throw new Error("Too many skipped messages");
    }
    const nostrSender = this.getNostrSenderKeypair(Sender.Them, KeyType.Current).publicKey;
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
  private handleNostrEvent(e: any, receivingNostrKey: KeyPair, first: boolean) {
    const header = JSON.parse(nip44.decrypt(e.tags[0][1], receivingNostrKey.privateKey)) as Header;
    const data = this.ratchetDecrypt(header, e.content, e.pubkey, first);
    this.internalSubscriptions.forEach(callback => callback({id: e.id, data, pubkey: header.nextPublicKey, time: header.time}));
    
    if (header.nextPublicKey !== this.state.theirCurrentNostrPublicKey) {
      this.nostrUnsubscribe?.();
      this.nostrUnsubscribe = this.nostrNextUnsubscribe;
      this.subscribeToNextNostrEvents();
    }
  }

  private subscribeToNostrEvents() {
    if (this.nostrUnsubscribe) return;
    
    const receivingNostrKey = this.getNostrSenderKeypair(Sender.Them, KeyType.Current);
    this.nostrUnsubscribe = this.nostrSubscribe(
      {authors: [receivingNostrKey.publicKey], kinds: [EVENT_KIND]},
      (e) => this.handleNostrEvent(e, receivingNostrKey, true)
    );
    
    this.subscribeToNextNostrEvents();
  }

  private subscribeToNextNostrEvents() {
    const nextReceivingNostrKey = this.getNostrSenderKeypair(Sender.Them, KeyType.Next);
    this.nostrNextUnsubscribe = this.nostrSubscribe(
      {authors: [nextReceivingNostrKey.publicKey], kinds: [EVENT_KIND]},
      (e) => this.handleNostrEvent(e, nextReceivingNostrKey, false)
    );
  }
}
