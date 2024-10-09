import { generateSecretKey, getPublicKey, nip44, finalizeEvent, VerifiedEvent } from "nostr-tools";
import { hexToBytes } from "@noble/hashes/utils";
import {
  ChannelState,
  RatchetMessage,
  Unsubscribe,
  NostrSubscribe,
  MessageCallback,
  EVENT_KIND,
  KeyPair,
  Sender,
  KeyType,
} from "./types";
import { kdf } from "./utils";

export class Channel {
  nostrUnsubscribe: Unsubscribe | undefined
  nostrNextUnsubscribe: Unsubscribe | undefined
  currentInternalSubscriptionId = 0
  internalSubscriptions = new Map<number, MessageCallback>()
  name = Math.random().toString(36).substring(2, 6)

  constructor(private nostrSubscribe: NostrSubscribe, public state: ChannelState) {
    this.name = Math.random().toString(36).substring(2, 6)
  }

  /**
   * To preserve forward secrecy, do not use long-term keys for channel initialization. Use e.g. InviteLink to exchange session keys.
   */
  static init(nostrSubscribe: NostrSubscribe, theirCurrentNostrPublicKey: string, ourCurrentPrivateKey: Uint8Array, name?: string): Channel {
    const ourNextPrivateKey = generateSecretKey()
    const state: ChannelState = {
      theirCurrentNostrPublicKey,
      ourCurrentNostrKey: { publicKey: getPublicKey(ourCurrentPrivateKey), privateKey: ourCurrentPrivateKey },
      ourNextNostrKey: { publicKey: getPublicKey(ourNextPrivateKey), privateKey: ourNextPrivateKey },
      receivingChainKey: new Uint8Array(),
      nextReceivingChainKey: new Uint8Array(),
      sendingChainKey: new Uint8Array(),
      sendingChainMessageNumber: 0,
      receivingChainMessageNumber: 0,
      previousSendingChainMessageCount: 0,
      skippedMessageKeys: {}
    }
    const channel = new Channel(nostrSubscribe, state)
    channel.updateTheirCurrentNostrPublicKey(theirCurrentNostrPublicKey)
    if (name) channel.name = name
    return channel
  }

  updateTheirCurrentNostrPublicKey(theirNewPublicKey: string) {
    this.state.theirCurrentNostrPublicKey = theirNewPublicKey
    this.state.previousSendingChainMessageCount = this.state.sendingChainMessageNumber
    this.state.sendingChainMessageNumber = 0
    this.state.receivingChainMessageNumber = 0
    this.state.receivingChainKey = this.getNostrSenderKeypair(Sender.Them, KeyType.Current).privateKey
    this.state.nextReceivingChainKey = this.getNostrSenderKeypair(Sender.Them, KeyType.Next).privateKey
    this.state.sendingChainKey = this.getNostrSenderKeypair(Sender.Us, KeyType.Current).privateKey
  }

  private rotateOurCurrentNostrKey() {
    this.state.ourCurrentNostrKey = this.state.ourNextNostrKey
    const ourNextSecretKey = generateSecretKey()
    this.state.ourNextNostrKey = {
      publicKey: getPublicKey(ourNextSecretKey),
      privateKey: ourNextSecretKey
    }
  }

  getNostrSenderKeypair(sender: Sender, keyType: KeyType): KeyPair {
    if (sender === Sender.Us && keyType === KeyType.Next) {
      throw new Error("We don't have their next key")
    }
    const ourPrivate = keyType === KeyType.Current ? this.state.ourCurrentNostrKey.privateKey : this.state.ourNextNostrKey.privateKey
    const theirPublic = this.state.theirCurrentNostrPublicKey
    const senderPubKey = sender === Sender.Us ? getPublicKey(ourPrivate) : theirPublic
    const privateKey = kdf(nip44.getConversationKey(ourPrivate, theirPublic), hexToBytes(senderPubKey))
    return {
      publicKey: getPublicKey(privateKey),
      privateKey
    }
  }

  private nostrSubscribeNext() {
    const nextReceivingPublicKey = this.getNostrSenderKeypair(Sender.Them, KeyType.Next).publicKey
    const decryptKey = this.state.nextReceivingChainKey
    this.nostrNextUnsubscribe = this.nostrSubscribe({authors: [nextReceivingPublicKey], kinds: [EVENT_KIND]}, (e) => {
      // they acknowledged our next key and sent with the corresponding new nostr sender key
      const msg = JSON.parse(nip44.decrypt(e.content, decryptKey)) as RatchetMessage
      if (msg.nextPublicKey !== this.state.theirCurrentNostrPublicKey) {
        this.rotateOurCurrentNostrKey()
        this.updateTheirCurrentNostrPublicKey(msg.nextPublicKey)
        this.nostrUnsubscribe?.()
        this.nostrUnsubscribe = this.nostrNextUnsubscribe
        this.nostrSubscribeNext()
      }
      this.internalSubscriptions.forEach(callback => callback({id: e.id, data: msg.data, pubkey: msg.nextPublicKey, time: msg.time}))
    })
  }

  private subscribeToNostrEvents() {
    if (this.nostrUnsubscribe) {
      return
    }
    const receivingPublicKey = this.getNostrSenderKeypair(Sender.Them, KeyType.Current).publicKey
    const decryptKey = this.state.receivingChainKey
    this.nostrUnsubscribe = this.nostrSubscribe({authors: [receivingPublicKey], kinds: [EVENT_KIND]}, (e) => {
      const msg = JSON.parse(nip44.decrypt(e.content, decryptKey)) as RatchetMessage
      if (msg.nextPublicKey !== this.state.theirCurrentNostrPublicKey) {
        // they announced their next key: we will use it to derive the next nostr sender key
        this.updateTheirCurrentNostrPublicKey(msg.nextPublicKey)
      }
      this.internalSubscriptions.forEach(callback => callback({id: e.id, data: msg.data, pubkey: msg.nextPublicKey, time: msg.time}))
    })
    this.nostrSubscribeNext()
  }

  onMessage(callback: MessageCallback): Unsubscribe {
    const id = this.currentInternalSubscriptionId++
    this.internalSubscriptions.set(id, callback)
    this.subscribeToNostrEvents()
    return () => this.internalSubscriptions.delete(id)
  }

  send(data: string): VerifiedEvent {
    const message: RatchetMessage = {
      number: this.state.sendingChainMessageNumber,
      data: data,
      nextPublicKey: this.state.ourNextNostrKey.publicKey,
      time: Date.now()
    }
    this.state.sendingChainMessageNumber++
    const sendingPrivateKey = this.getNostrSenderKeypair(Sender.Us, KeyType.Current).privateKey
    const encryptedData = nip44.encrypt(JSON.stringify(message), this.state.sendingChainKey)
    const nostrEvent = finalizeEvent({
      content: encryptedData,
      kind: EVENT_KIND,
      tags: [],
      created_at: Math.floor(Date.now() / 1000)
    }, sendingPrivateKey)

    return nostrEvent
  }
}
