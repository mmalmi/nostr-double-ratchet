import { generateSecretKey, getPublicKey, VerifiedEvent } from "nostr-tools"
import { bytesToHex } from "@noble/hashes/utils"
import { InviteList, DeviceEntry } from "./InviteList"
import { DevicePayload } from "./inviteUtils"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, Unsubscribe, IdentityKey } from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { SessionManager } from "./SessionManager"

/**
 * Options for creating an OwnerDeviceManager
 *
 * For extension login (NIP-07), pass { encrypt, decrypt } functions instead of raw private key.
 */
export interface OwnerDeviceOptions {
  ownerPublicKey: string
  /** Raw private key bytes OR { encrypt, decrypt } functions for extension login */
  identityKey: IdentityKey
  deviceId: string
  deviceLabel: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Options for creating a DelegateDeviceManager
 */
export interface DelegateDeviceOptions {
  deviceId: string
  deviceLabel: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Options for restoring a delegate device from existing credentials
 */
export interface RestoreDelegateOptions {
  deviceId: string
  deviceLabel: string
  devicePublicKey: string
  devicePrivateKey: Uint8Array
  ephemeralPublicKey: string
  ephemeralPrivateKey: Uint8Array
  sharedSecret: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Result from creating a delegate device
 */
export interface CreateDelegateResult {
  manager: DelegateDeviceManager
  payload: DevicePayload
}

/**
 * Common interface for device managers
 */
export interface IDeviceManager {
  init(): Promise<void>
  getDeviceId(): string
  getDeviceLabel(): string
  getIdentityPublicKey(): string
  getIdentityKey(): IdentityKey
  getEphemeralKeypair(): { publicKey: string; privateKey: Uint8Array } | null
  getSharedSecret(): string | null
  getOwnerPublicKey(): string | null
  close(): void
  createSessionManager(sessionStorage?: StorageAdapter): SessionManager
}

/**
 * OwnerDeviceManager handles device lifecycle for the owner's main device.
 * Has owner's identity key, can manage InviteList (add/revoke devices).
 */
export class OwnerDeviceManager implements IDeviceManager {
  private readonly deviceId: string
  private readonly deviceLabel: string
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter
  private readonly ownerPublicKey: string
  private readonly identityKey: IdentityKey

  private inviteList: InviteList | null = null
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "1"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  constructor(options: OwnerDeviceOptions) {
    this.deviceId = options.deviceId
    this.deviceLabel = options.deviceLabel
    this.nostrSubscribe = options.nostrSubscribe
    this.nostrPublish = options.nostrPublish
    this.storage = options.storage || new InMemoryStorageAdapter()
    this.ownerPublicKey = options.ownerPublicKey
    this.identityKey = options.identityKey
  }

  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    // 1. Load from local storage
    const local = await this.loadInviteList()

    // 2. Fetch from relay
    const remote = await this.fetchInviteList(this.ownerPublicKey)

    // 3. Merge local + remote
    const inviteList = this.mergeInviteLists(local, remote)

    // 4. Add our device if not present
    if (!inviteList.getDevice(this.deviceId)) {
      const device = inviteList.createDevice(this.deviceLabel, this.deviceId)
      inviteList.addDevice(device)
    }

    // 5. Save and set
    this.inviteList = inviteList
    await this.saveInviteList(inviteList)

    // 6. Publish
    const event = inviteList.getEvent()
    await this.nostrPublish(event).catch((error) => {
      console.error("Failed to publish InviteList:", error)
    })

    // 7. Subscribe to our InviteList for updates
    this.subscribeToOwnInviteList()
  }

  getDeviceId(): string {
    return this.deviceId
  }

  getDeviceLabel(): string {
    return this.deviceLabel
  }

  getIdentityPublicKey(): string {
    return this.ownerPublicKey
  }

  getIdentityKey(): IdentityKey {
    return this.identityKey
  }

  /**
   * Check if this DeviceManager is using extension login (function callbacks instead of raw keys)
   */
  isExtensionLogin(): boolean {
    return !(this.identityKey instanceof Uint8Array)
  }

  getEphemeralKeypair(): { publicKey: string; privateKey: Uint8Array } | null {
    const device = this.inviteList?.getDevice(this.deviceId)
    if (!device?.ephemeralPublicKey || !device?.ephemeralPrivateKey) {
      return null
    }
    return {
      publicKey: device.ephemeralPublicKey,
      privateKey: device.ephemeralPrivateKey,
    }
  }

  getSharedSecret(): string | null {
    const device = this.inviteList?.getDevice(this.deviceId)
    return device?.sharedSecret || null
  }

  getOwnerPublicKey(): string {
    return this.ownerPublicKey
  }

  /**
   * Get the InviteList
   */
  getInviteList(): InviteList | null {
    return this.inviteList
  }

  /**
   * Get all devices from the InviteList
   */
  getOwnDevices(): DeviceEntry[] {
    return this.inviteList?.getAllDevices() || []
  }

  /**
   * Add a device to the InviteList
   */
  async addDevice(payload: DevicePayload): Promise<void> {
    await this.init()

    await this.modifyInviteList((list) => {
      const device: DeviceEntry = {
        ephemeralPublicKey: payload.ephemeralPubkey,
        sharedSecret: payload.sharedSecret,
        deviceId: payload.deviceId,
        deviceLabel: payload.deviceLabel,
        createdAt: Math.floor(Date.now() / 1000),
        identityPubkey: payload.identityPubkey,
      }
      list.addDevice(device)
    })
  }

  /**
   * Revoke a device from the InviteList
   */
  async revokeDevice(deviceId: string): Promise<void> {
    if (deviceId === this.deviceId) {
      throw new Error("Cannot revoke own device")
    }

    await this.init()

    await this.modifyInviteList((list) => {
      list.removeDevice(deviceId)
    })
  }

  /**
   * Update a device's label
   */
  async updateDeviceLabel(deviceId: string, label: string): Promise<void> {
    await this.init()

    await this.modifyInviteList((list) => {
      list.updateDeviceLabel(deviceId, label)
    })
  }

  close(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe()
    }
    this.subscriptions = []
  }

  /**
   * Creates a SessionManager configured for this device.
   * Must be called after init().
   */
  createSessionManager(sessionStorage?: StorageAdapter): SessionManager {
    if (!this.initialized) {
      throw new Error("DeviceManager must be initialized before creating SessionManager")
    }

    const ephemeralKeypair = this.getEphemeralKeypair()
    const sharedSecret = this.getSharedSecret()

    if (!ephemeralKeypair || !sharedSecret) {
      throw new Error("Ephemeral keypair and shared secret required for SessionManager")
    }

    return new SessionManager(
      this.ownerPublicKey,
      this.identityKey,
      this.deviceId,
      this.nostrSubscribe,
      this.nostrPublish,
      this.ownerPublicKey,
      { ephemeralKeypair, sharedSecret },
      sessionStorage || this.storage,
    )
  }

  // Private helpers

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

  private fetchInviteList(pubkey: string, timeoutMs = 500): Promise<InviteList | null> {
    return new Promise((resolve) => {
      let latestEvent: { event: VerifiedEvent; inviteList: InviteList } | null = null
      let resolved = false

      setTimeout(() => {
        if (resolved) return
        resolved = true
        unsubscribe()
        resolve(latestEvent?.inviteList ?? null)
      }, timeoutMs)

      let unsubscribe: () => void = () => {}
      unsubscribe = this.nostrSubscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          authors: [pubkey],
          "#d": ["double-ratchet/invite-list"],
        },
        (event) => {
          if (resolved) return
          try {
            const inviteList = InviteList.fromEvent(event)
            // Keep track of the latest event by created_at
            if (!latestEvent || event.created_at >= latestEvent.event.created_at) {
              latestEvent = { event, inviteList }
            }
          } catch {
            // Invalid event, ignore
          }
        }
      )

      // If found synchronously, unsubscribe
      if (resolved) {
        unsubscribe()
      }
    })
  }

  private mergeInviteLists(local: InviteList | null, remote: InviteList | null): InviteList {
    if (local && remote) return local.merge(remote)
    if (local) return local
    if (remote) return remote
    return new InviteList(this.ownerPublicKey)
  }

  private async modifyInviteList(change: (list: InviteList) => void): Promise<void> {
    // Fetch latest from relay
    const remote = await this.fetchInviteList(this.ownerPublicKey)

    // Merge with local
    const merged = this.mergeInviteLists(this.inviteList, remote)

    // Apply change
    change(merged)

    // Publish and save
    const event = merged.getEvent()
    await this.nostrPublish(event)
    await this.saveInviteList(merged)

    this.inviteList = merged
  }

  private subscribeToOwnInviteList(): void {
    const unsubscribe = this.nostrSubscribe(
      {
        kinds: [INVITE_LIST_EVENT_KIND],
        authors: [this.ownerPublicKey],
        "#d": ["double-ratchet/invite-list"],
      },
      (event) => {
        try {
          const remote = InviteList.fromEvent(event)
          if (this.inviteList) {
            this.inviteList = this.inviteList.merge(remote)
            this.saveInviteList(this.inviteList).catch(console.error)
          }
        } catch {
          // Invalid event, ignore
        }
      }
    )

    this.subscriptions.push(unsubscribe)
  }
}

/**
 * DelegateDeviceManager handles device lifecycle for delegate devices.
 * Has own identity key, waits for activation, checks revocation status.
 */
export class DelegateDeviceManager implements IDeviceManager {
  private readonly deviceId: string
  private readonly deviceLabel: string
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter

  private readonly devicePublicKey: string
  private readonly devicePrivateKey: Uint8Array
  private readonly ephemeralPublicKey: string
  private readonly ephemeralPrivateKey: Uint8Array
  private readonly sharedSecret: string

  private ownerPubkeyFromActivation?: string
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "1"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  private constructor(
    deviceId: string,
    deviceLabel: string,
    nostrSubscribe: NostrSubscribe,
    nostrPublish: NostrPublish,
    storage: StorageAdapter,
    devicePublicKey: string,
    devicePrivateKey: Uint8Array,
    ephemeralPublicKey: string,
    ephemeralPrivateKey: Uint8Array,
    sharedSecret: string,
  ) {
    this.deviceId = deviceId
    this.deviceLabel = deviceLabel
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.storage = storage
    this.devicePublicKey = devicePublicKey
    this.devicePrivateKey = devicePrivateKey
    this.ephemeralPublicKey = ephemeralPublicKey
    this.ephemeralPrivateKey = ephemeralPrivateKey
    this.sharedSecret = sharedSecret
  }

  /**
   * Create a new delegate device (generates own keys)
   */
  static create(options: DelegateDeviceOptions): CreateDelegateResult {
    // Generate identity keypair for this delegate device
    const devicePrivateKey = generateSecretKey()
    const devicePublicKey = getPublicKey(devicePrivateKey)

    // Generate ephemeral keypair for invite handshakes
    const ephemeralPrivateKey = generateSecretKey()
    const ephemeralPublicKey = getPublicKey(ephemeralPrivateKey)

    // Generate shared secret
    const sharedSecret = bytesToHex(generateSecretKey())

    const manager = new DelegateDeviceManager(
      options.deviceId,
      options.deviceLabel,
      options.nostrSubscribe,
      options.nostrPublish,
      options.storage || new InMemoryStorageAdapter(),
      devicePublicKey,
      devicePrivateKey,
      ephemeralPublicKey,
      ephemeralPrivateKey,
      sharedSecret,
    )

    const payload: DevicePayload = {
      ephemeralPubkey: ephemeralPublicKey,
      sharedSecret,
      deviceId: options.deviceId,
      deviceLabel: options.deviceLabel,
      identityPubkey: devicePublicKey,
    }

    return { manager, payload }
  }

  /**
   * Restore a delegate device from existing credentials
   */
  static restore(options: RestoreDelegateOptions): DelegateDeviceManager {
    return new DelegateDeviceManager(
      options.deviceId,
      options.deviceLabel,
      options.nostrSubscribe,
      options.nostrPublish,
      options.storage || new InMemoryStorageAdapter(),
      options.devicePublicKey,
      options.devicePrivateKey,
      options.ephemeralPublicKey,
      options.ephemeralPrivateKey,
      options.sharedSecret,
    )
  }

  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    // Load stored owner pubkey if exists
    const storedOwnerPubkey = await this.storage.get<string>(this.ownerPubkeyKey())
    if (storedOwnerPubkey) {
      this.ownerPubkeyFromActivation = storedOwnerPubkey
    }
  }

  getDeviceId(): string {
    return this.deviceId
  }

  getDeviceLabel(): string {
    return this.deviceLabel
  }

  getIdentityPublicKey(): string {
    return this.devicePublicKey
  }

  getIdentityKey(): Uint8Array {
    return this.devicePrivateKey
  }

  getEphemeralKeypair(): { publicKey: string; privateKey: Uint8Array } {
    return {
      publicKey: this.ephemeralPublicKey,
      privateKey: this.ephemeralPrivateKey,
    }
  }

  getSharedSecret(): string {
    return this.sharedSecret
  }

  getOwnerPublicKey(): string | null {
    return this.ownerPubkeyFromActivation || null
  }

  /**
   * Wait for this delegate device to be activated (added to an InviteList)
   * Returns the owner's public key
   */
  async waitForActivation(timeoutMs = 60000): Promise<string> {
    // If already activated, return immediately
    if (this.ownerPubkeyFromActivation) {
      return this.ownerPubkeyFromActivation
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        unsubscribe()
        reject(new Error("Activation timeout"))
      }, timeoutMs)

      // Subscribe to all InviteList events and look for our deviceId
      const unsubscribe = this.nostrSubscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          "#d": ["double-ratchet/invite-list"],
        },
        async (event) => {
          try {
            const inviteList = InviteList.fromEvent(event)
            const device = inviteList.getDevice(this.deviceId)

            if (device && device.ephemeralPublicKey === this.ephemeralPublicKey) {
              // Found our device in someone's InviteList
              clearTimeout(timeout)
              unsubscribe()

              this.ownerPubkeyFromActivation = event.pubkey
              await this.storage.put(this.ownerPubkeyKey(), event.pubkey)

              resolve(event.pubkey)
            }
          } catch {
            // Invalid InviteList, ignore
          }
        }
      )

      this.subscriptions.push(unsubscribe)
    })
  }

  /**
   * Check if this delegate device has been revoked
   */
  async isRevoked(): Promise<boolean> {
    const ownerPubkey = this.getOwnerPublicKey()
    if (!ownerPubkey) {
      return false // Not activated yet
    }

    const inviteList = await this.fetchInviteList(ownerPubkey)
    if (!inviteList) {
      return true // No InviteList found, assume revoked
    }

    const device = inviteList.getDevice(this.deviceId)
    return !device || device.ephemeralPublicKey !== this.ephemeralPublicKey
  }

  close(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe()
    }
    this.subscriptions = []
  }

  /**
   * Creates a SessionManager configured for this device.
   * Must be called after init() and after activation.
   */
  createSessionManager(sessionStorage?: StorageAdapter): SessionManager {
    if (!this.initialized) {
      throw new Error("DeviceManager must be initialized before creating SessionManager")
    }

    const ownerPublicKey = this.getOwnerPublicKey()
    if (!ownerPublicKey) {
      throw new Error("Owner public key required for SessionManager - device must be activated first")
    }

    return new SessionManager(
      this.devicePublicKey,
      this.devicePrivateKey,
      this.deviceId,
      this.nostrSubscribe,
      this.nostrPublish,
      ownerPublicKey,
      {
        ephemeralKeypair: {
          publicKey: this.ephemeralPublicKey,
          privateKey: this.ephemeralPrivateKey,
        },
        sharedSecret: this.sharedSecret,
      },
      sessionStorage || this.storage,
    )
  }

  // Private helpers

  private ownerPubkeyKey(): string {
    return `${this.versionPrefix}/device-manager/owner-pubkey`
  }

  private fetchInviteList(pubkey: string, timeoutMs = 500): Promise<InviteList | null> {
    return new Promise((resolve) => {
      let latestEvent: { event: VerifiedEvent; inviteList: InviteList } | null = null
      let resolved = false

      setTimeout(() => {
        if (resolved) return
        resolved = true
        unsubscribe()
        resolve(latestEvent?.inviteList ?? null)
      }, timeoutMs)

      let unsubscribe: () => void = () => {}
      unsubscribe = this.nostrSubscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          authors: [pubkey],
          "#d": ["double-ratchet/invite-list"],
        },
        (event) => {
          if (resolved) return
          try {
            const inviteList = InviteList.fromEvent(event)
            if (!latestEvent || event.created_at >= latestEvent.event.created_at) {
              latestEvent = { event, inviteList }
            }
          } catch {
            // Invalid event, ignore
          }
        }
      )

      if (resolved) {
        unsubscribe()
      }
    })
  }
}
