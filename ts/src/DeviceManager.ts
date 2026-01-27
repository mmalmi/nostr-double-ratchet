import { generateSecretKey, getPublicKey } from "nostr-tools"
import { InviteList, DeviceEntry } from "./InviteList"
import { Invite } from "./Invite"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, Unsubscribe } from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { SessionManager } from "./SessionManager"

export interface DelegatePayload {
  identityPubkey: string
}

/**
 * Options for DeviceManager (authority for InviteList)
 */
export interface DeviceManagerOptions {
  ownerPublicKey: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Options for DelegateManager (device identity)
 */
export interface DelegateManagerOptions {
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Options for restoring a DelegateManager from stored keys
 */
export interface RestoreDelegateManagerOptions {
  devicePublicKey: string
  devicePrivateKey: Uint8Array
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Result of creating a new DelegateManager
 */
export interface CreateDelegateManagerResult {
  manager: DelegateManager
  payload: DelegatePayload
}

/**
 * DeviceManager - Authority for InviteList.
 * Uses main key ONLY for signing InviteList events.
 * Does NOT have device identity (no Invite, no SessionManager creation).
 */
export class DeviceManager {
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter
  private readonly ownerPublicKey: string

  private inviteList: InviteList | null = null
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "3"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  constructor(options: DeviceManagerOptions) {
    this.nostrSubscribe = options.nostrSubscribe
    this.nostrPublish = options.nostrPublish
    this.storage = options.storage || new InMemoryStorageAdapter()
    this.ownerPublicKey = options.ownerPublicKey
  }

  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    // Load local only - no auto-subscribe, no auto-publish, no auto-merge
    this.inviteList = await this.loadInviteList()
    if (!this.inviteList) {
      this.inviteList = new InviteList(this.ownerPublicKey)
    }
  }

  getOwnerPublicKey(): string {
    return this.ownerPublicKey
  }

  getInviteList(): InviteList | null {
    return this.inviteList
  }

  getOwnDevices(): DeviceEntry[] {
    return this.inviteList?.getAllDevices() || []
  }

  /**
   * Add a device to the InviteList.
   * Only adds identity info - the device publishes its own Invite separately.
   * This is a local-only operation - call publish() to publish to relays.
   */
  addDevice(payload: DelegatePayload): void {
    if (!this.inviteList) {
      this.inviteList = new InviteList(this.ownerPublicKey)
    }

    const device: DeviceEntry = {
      identityPubkey: payload.identityPubkey,
      createdAt: Math.floor(Date.now() / 1000),
    }
    this.inviteList.addDevice(device)
    this.saveInviteList(this.inviteList).catch(console.error)
  }

  /**
   * Revoke a device from the InviteList.
   * This is a local-only operation - call publish() to publish to relays.
   */
  revokeDevice(identityPubkey: string): void {
    if (!this.inviteList) return

    this.inviteList.removeDevice(identityPubkey)
    this.saveInviteList(this.inviteList).catch(console.error)
  }

  /**
   * Publish the current InviteList to relays.
   * This is the only way to publish - addDevice/revokeDevice are local-only.
   */
  async publish(): Promise<void> {
    if (!this.inviteList) {
      this.inviteList = new InviteList(this.ownerPublicKey)
    }

    const event = this.inviteList.getEvent()
    await this.nostrPublish(event)
  }

  /**
   * Replace the local InviteList with the given list and save to storage.
   * Used for authority transfer - receive list from another device, then call publish().
   */
  async setInviteList(list: InviteList): Promise<void> {
    this.inviteList = list
    await this.saveInviteList(list)
  }

  /**
   * Subscribe to InviteList events from own account.
   * Client can use this to receive updates, but merging is client responsibility.
   */
  subscribeToOwnInviteList(onInviteList: (list: InviteList) => void): void {
    const unsubscribe = InviteList.fromUser(
      this.ownerPublicKey,
      this.nostrSubscribe,
      onInviteList
    )
    this.subscriptions.push(unsubscribe)
  }

  close(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe()
    }
    this.subscriptions = []
  }

  private inviteListKey(): string {
    return `${this.versionPrefix}/device-manager/invite-list`
  }

  private async loadInviteList(): Promise<InviteList | null> {
    const data = await this.storage.get<string>(this.inviteListKey())
    if (!data) return null
    try {
      return InviteList.deserialize(data)
    } catch {
      return null
    }
  }

  private async saveInviteList(list: InviteList): Promise<void> {
    await this.storage.put(this.inviteListKey(), list.serialize())
  }
}

/**
 * DelegateManager - Device identity manager.
 * ALL devices (including main) use this for their device identity.
 * Publishes own Invite events, used for SessionManager DH encryption.
 */
export class DelegateManager {
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter

  private readonly devicePublicKey: string
  private readonly devicePrivateKey: Uint8Array

  private invite: Invite | null = null
  private ownerPubkeyFromActivation?: string
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "1"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  protected constructor(
    nostrSubscribe: NostrSubscribe,
    nostrPublish: NostrPublish,
    storage: StorageAdapter,
    devicePublicKey: string,
    devicePrivateKey: Uint8Array,
  ) {
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.storage = storage
    this.devicePublicKey = devicePublicKey
    this.devicePrivateKey = devicePrivateKey
  }

  /**
   * Create a new DelegateManager with fresh identity keys.
   */
  static create(options: DelegateManagerOptions): CreateDelegateManagerResult {
    const devicePrivateKey = generateSecretKey()
    const devicePublicKey = getPublicKey(devicePrivateKey)

    const manager = new DelegateManager(
      options.nostrSubscribe,
      options.nostrPublish,
      options.storage || new InMemoryStorageAdapter(),
      devicePublicKey,
      devicePrivateKey,
    )

    // Simplified payload - only identity pubkey needed
    const payload: DelegatePayload = {
      identityPubkey: devicePublicKey,
    }

    return { manager, payload }
  }

  /**
   * Restore a DelegateManager from stored keys.
   */
  static restore(options: RestoreDelegateManagerOptions): DelegateManager {
    return new DelegateManager(
      options.nostrSubscribe,
      options.nostrPublish,
      options.storage || new InMemoryStorageAdapter(),
      options.devicePublicKey,
      options.devicePrivateKey,
    )
  }

  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    const storedOwnerPubkey = await this.storage.get<string>(this.ownerPubkeyKey())
    if (storedOwnerPubkey) {
      this.ownerPubkeyFromActivation = storedOwnerPubkey
    }

    // Load or create Invite for this device
    const savedInvite = await this.loadInvite()
    this.invite = savedInvite || Invite.createNew(this.devicePublicKey, this.devicePublicKey)
    await this.saveInvite(this.invite)

    // Publish Invite event (signed by this device's identity key)
    const inviteEvent = this.invite.getEvent()
    await this.nostrPublish(inviteEvent).catch((error) => {
      console.error("Failed to publish Invite:", error)
    })
  }

  getIdentityPublicKey(): string {
    return this.devicePublicKey
  }

  getIdentityKey(): Uint8Array {
    return this.devicePrivateKey
  }

  getInvite(): Invite | null {
    return this.invite
  }

  getOwnerPublicKey(): string | null {
    return this.ownerPubkeyFromActivation || null
  }

  /**
   * Rotate this device's invite - generates new ephemeral keys and shared secret.
   */
  async rotateInvite(): Promise<void> {
    await this.init()

    this.invite = Invite.createNew(this.devicePublicKey, this.devicePublicKey)
    await this.saveInvite(this.invite)

    const inviteEvent = this.invite.getEvent()
    await this.nostrPublish(inviteEvent)
  }

  /**
   * Activate this device with a known owner.
   * Use this when you know the device has been added (e.g., main device adding itself).
   * Skips fetching from relay - just stores the owner pubkey.
   */
  async activate(ownerPublicKey: string): Promise<void> {
    this.ownerPubkeyFromActivation = ownerPublicKey
    await this.storage.put(this.ownerPubkeyKey(), ownerPublicKey)
  }

  /**
   * Wait for this device to be activated (added to an InviteList).
   * Returns the owner's public key once activated.
   * For delegate devices that don't know the owner ahead of time.
   */
  async waitForActivation(timeoutMs = 60000): Promise<string> {
    if (this.ownerPubkeyFromActivation) {
      return this.ownerPubkeyFromActivation
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        unsubscribe()
        reject(new Error("Activation timeout"))
      }, timeoutMs)

      // Subscribe to all InviteList events and look for our identityPubkey
      const unsubscribe = this.nostrSubscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          "#d": ["double-ratchet/invite-list"],
        },
        async (event) => {
          try {
            const inviteList = InviteList.fromEvent(event)
            const device = inviteList.getDevice(this.devicePublicKey)

            // Check that our identity pubkey is in the list
            if (device) {
              clearTimeout(timeout)
              unsubscribe()
              this.ownerPubkeyFromActivation = event.pubkey
              await this.storage.put(this.ownerPubkeyKey(), event.pubkey)
              resolve(event.pubkey)
            }
          } catch {
            // Invalid InviteList
          }
        }
      )

      this.subscriptions.push(unsubscribe)
    })
  }

  /**
   * Check if this device has been revoked from the owner's InviteList.
   */
  async isRevoked(): Promise<boolean> {
    const ownerPubkey = this.getOwnerPublicKey()
    if (!ownerPubkey) return false

    const inviteList = await InviteList.waitFor(ownerPubkey, this.nostrSubscribe, 500)
    if (!inviteList) return true

    const device = inviteList.getDevice(this.devicePublicKey)
    // Device is revoked if not in list
    return !device
  }

  close(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe()
    }
    this.subscriptions = []
  }

  /**
   * Create a SessionManager for this device.
   */
  createSessionManager(sessionStorage?: StorageAdapter): SessionManager {
    if (!this.initialized) {
      throw new Error("DelegateManager must be initialized before creating SessionManager")
    }

    const ownerPublicKey = this.getOwnerPublicKey()
    if (!ownerPublicKey) {
      throw new Error("Owner public key required for SessionManager - device must be activated first")
    }

    if (!this.invite || !this.invite.inviterEphemeralPrivateKey) {
      throw new Error("Invite with ephemeral keys required for SessionManager")
    }

    const ephemeralKeypair = {
      publicKey: this.invite.inviterEphemeralPublicKey,
      privateKey: this.invite.inviterEphemeralPrivateKey,
    }
    const sharedSecret = this.invite.sharedSecret

    return new SessionManager(
      this.devicePublicKey,
      this.devicePrivateKey,
      this.devicePublicKey, // Use identityPubkey as deviceId
      this.nostrSubscribe,
      this.nostrPublish,
      ownerPublicKey,
      { ephemeralKeypair, sharedSecret },
      sessionStorage || this.storage,
    )
  }

  private ownerPubkeyKey(): string {
    return `${this.versionPrefix}/device-manager/owner-pubkey`
  }

  private inviteKey(): string {
    return `${this.versionPrefix}/device-manager/invite`
  }

  private async loadInvite(): Promise<Invite | null> {
    const data = await this.storage.get<string>(this.inviteKey())
    if (!data) return null
    try {
      return Invite.deserialize(data)
    } catch {
      return null
    }
  }

  private async saveInvite(invite: Invite): Promise<void> {
    await this.storage.put(this.inviteKey(), invite.serialize())
  }
}

