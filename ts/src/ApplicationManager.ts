import { generateSecretKey, getPublicKey } from "nostr-tools"
import { ApplicationKeys, DeviceEntry } from "./ApplicationKeys"
import { Invite } from "./Invite"
import { NostrSubscribe, NostrPublish, APPLICATION_KEYS_EVENT_KIND, Unsubscribe } from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { SessionManager } from "./SessionManager"

export interface DelegatePayload {
  identityPubkey: string
}

/**
 * Options for ApplicationManager (authority for ApplicationKeys)
 */
export interface ApplicationManagerOptions {
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
 * ApplicationManager - Authority for ApplicationKeys.
 * Manages local ApplicationKeys and publishes to relays.
 * Does NOT have device identity (no Invite, no SessionManager creation).
 */
export class ApplicationManager {
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter

  private applicationKeys: ApplicationKeys | null = null
  private initialized = false

  private readonly storageVersion = "3"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  constructor(options: ApplicationManagerOptions) {
    this.nostrPublish = options.nostrPublish
    this.storage = options.storage || new InMemoryStorageAdapter()
  }

  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    // Load local only - no auto-subscribe, no auto-publish, no auto-merge
    this.applicationKeys = await this.loadApplicationKeys()
    if (!this.applicationKeys) {
      this.applicationKeys = new ApplicationKeys()
    }
  }

  getApplicationKeys(): ApplicationKeys | null {
    return this.applicationKeys
  }

  getOwnDevices(): DeviceEntry[] {
    return this.applicationKeys?.getAllDevices() || []
  }

  /**
   * Add a device to the ApplicationKeys.
   * Only adds identity info - the device publishes its own Invite separately.
   * This is a local-only operation - call publish() to publish to relays.
   */
  addDevice(payload: DelegatePayload): void {
    if (!this.applicationKeys) {
      this.applicationKeys = new ApplicationKeys()
    }

    const device: DeviceEntry = {
      identityPubkey: payload.identityPubkey,
      createdAt: Math.floor(Date.now() / 1000),
    }
    this.applicationKeys.addDevice(device)
    this.saveApplicationKeys(this.applicationKeys).catch(console.error)
  }

  /**
   * Revoke a device from the ApplicationKeys.
   * This is a local-only operation - call publish() to publish to relays.
   */
  revokeDevice(identityPubkey: string): void {
    if (!this.applicationKeys) return

    this.applicationKeys.removeDevice(identityPubkey)
    this.saveApplicationKeys(this.applicationKeys).catch(console.error)
  }

  /**
   * Publish the current ApplicationKeys to relays.
   * This is the only way to publish - addDevice/revokeDevice are local-only.
   */
  async publish(): Promise<void> {
    if (!this.applicationKeys) {
      this.applicationKeys = new ApplicationKeys()
    }

    const event = this.applicationKeys.getEvent()
    await this.nostrPublish(event)
  }

  /**
   * Replace the local ApplicationKeys with the given list and save to storage.
   * Used for authority transfer - receive list from another device, then call publish().
   */
  async setApplicationKeys(list: ApplicationKeys): Promise<void> {
    this.applicationKeys = list
    await this.saveApplicationKeys(list)
  }

  /**
   * Cleanup resources. Currently a no-op but kept for API consistency.
   */
  close(): void {
    // No-op - no subscriptions to clean up
  }

  private applicationKeysKey(): string {
    return `${this.versionPrefix}/application-manager/application-keys`
  }

  private async loadApplicationKeys(): Promise<ApplicationKeys | null> {
    const data = await this.storage.get<string>(this.applicationKeysKey())
    if (!data) return null
    try {
      return ApplicationKeys.deserialize(data)
    } catch {
      return null
    }
  }

  private async saveApplicationKeys(list: ApplicationKeys): Promise<void> {
    await this.storage.put(this.applicationKeysKey(), list.serialize())
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

  private devicePublicKey: string = ""
  private devicePrivateKey: Uint8Array = new Uint8Array()

  private invite: Invite | null = null
  private ownerPubkeyFromActivation?: string
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "1"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  constructor(options: DelegateManagerOptions) {
    this.nostrSubscribe = options.nostrSubscribe
    this.nostrPublish = options.nostrPublish
    this.storage = options.storage || new InMemoryStorageAdapter()
  }

  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    // Load or generate identity keys
    const storedPublicKey = await this.storage.get<string>(this.identityPublicKeyKey())
    const storedPrivateKey = await this.storage.get<number[]>(this.identityPrivateKeyKey())

    if (storedPublicKey && storedPrivateKey) {
      this.devicePublicKey = storedPublicKey
      this.devicePrivateKey = new Uint8Array(storedPrivateKey)
    } else {
      this.devicePrivateKey = generateSecretKey()
      this.devicePublicKey = getPublicKey(this.devicePrivateKey)
      await this.storage.put(this.identityPublicKeyKey(), this.devicePublicKey)
      await this.storage.put(this.identityPrivateKeyKey(), Array.from(this.devicePrivateKey))
    }

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

  /**
   * Get the registration payload for adding this device to an ApplicationManager.
   * Must be called after init().
   */
  getRegistrationPayload(): DelegatePayload {
    return { identityPubkey: this.devicePublicKey }
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
   * Wait for this device to be activated (added to an ApplicationKeys).
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

      // Subscribe to all ApplicationKeys events and look for our identityPubkey
      const unsubscribe = this.nostrSubscribe(
        {
          kinds: [APPLICATION_KEYS_EVENT_KIND],
          "#d": ["double-ratchet/application-keys"],
        },
        async (event) => {
          try {
            const applicationKeys = ApplicationKeys.fromEvent(event)
            const device = applicationKeys.getDevice(this.devicePublicKey)

            // Check that our identity pubkey is in the list
            if (device) {
              clearTimeout(timeout)
              unsubscribe()
              this.ownerPubkeyFromActivation = event.pubkey
              await this.storage.put(this.ownerPubkeyKey(), event.pubkey)
              resolve(event.pubkey)
            }
          } catch {
            // Invalid ApplicationKeys
          }
        }
      )

      this.subscriptions.push(unsubscribe)
    })
  }

  /**
   * Check if this device has been revoked from the owner's ApplicationKeys.
   * @param options.timeoutMs - Timeout for each attempt (default 2000ms)
   * @param options.retries - Number of retry attempts (default 2)
   */
  async isRevoked(options: { timeoutMs?: number; retries?: number } = {}): Promise<boolean> {
    const { timeoutMs = 2000, retries = 2 } = options
    const ownerPubkey = this.getOwnerPublicKey()
    if (!ownerPubkey) return false

    // Retry loop to handle slow relays
    for (let attempt = 0; attempt <= retries; attempt++) {
      const applicationKeys = await ApplicationKeys.waitFor(ownerPubkey, this.nostrSubscribe, timeoutMs)
      if (applicationKeys) {
        const device = applicationKeys.getDevice(this.devicePublicKey)
        // Device is revoked if not in list
        return !device
      }
      // No ApplicationKeys found - retry if we have attempts left
      if (attempt < retries) {
        console.log(`[isRevoked] No ApplicationKeys found, retrying (attempt ${attempt + 1}/${retries})`)
      }
    }

    // No ApplicationKeys found after all retries - assume revoked
    return true
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

  private identityPublicKeyKey(): string {
    return `${this.versionPrefix}/device-manager/identity-public-key`
  }

  private identityPrivateKeyKey(): string {
    return `${this.versionPrefix}/device-manager/identity-private-key`
  }
}

