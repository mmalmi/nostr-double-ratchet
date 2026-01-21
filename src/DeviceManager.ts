import { generateSecretKey, getPublicKey, VerifiedEvent } from "nostr-tools"
import { bytesToHex } from "@noble/hashes/utils"
import { InviteList, DeviceEntry } from "./InviteList"
import { DevicePayload } from "./inviteUtils"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, Unsubscribe, IdentityKey } from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { SessionManager } from "./SessionManager"

/**
 * Options for creating a DeviceManager with owner's key (can manage InviteList)
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
 * Options for creating a delegate device DeviceManager
 */
export interface DelegateDeviceOptions {
  deviceId: string
  deviceLabel: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

/**
 * Result from creating a delegate device
 */
export interface CreateDelegateResult {
  manager: DeviceManager
  payload: DevicePayload
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
 * DeviceManager handles device lifecycle and InviteList management.
 *
 * Owner mode: Has owner's nsec, can manage InviteList (add/revoke devices)
 * Delegate mode: Has own identity key, waits for activation, checks revocation status
 */
export class DeviceManager {
  private readonly delegateMode: boolean
  private readonly deviceId: string
  private readonly deviceLabel: string
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter

  // Owner mode fields (has owner's nsec or encrypt/decrypt functions)
  private readonly ownerPublicKey?: string
  private readonly identityKey?: IdentityKey

  // Delegate mode fields
  private readonly devicePublicKey?: string
  private readonly devicePrivateKey?: Uint8Array
  private readonly ephemeralPublicKey?: string
  private readonly ephemeralPrivateKey?: Uint8Array
  private readonly sharedSecret?: string
  private ownerPubkeyFromActivation?: string

  // Shared state
  private inviteList: InviteList | null = null
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  // Storage keys
  private readonly storageVersion = "1"
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  private constructor(options: {
    delegateMode: boolean
    deviceId: string
    deviceLabel: string
    nostrSubscribe: NostrSubscribe
    nostrPublish: NostrPublish
    storage?: StorageAdapter
    ownerPublicKey?: string
    identityKey?: IdentityKey
    devicePublicKey?: string
    devicePrivateKey?: Uint8Array
    ephemeralPublicKey?: string
    ephemeralPrivateKey?: Uint8Array
    sharedSecret?: string
  }) {
    this.delegateMode = options.delegateMode
    this.deviceId = options.deviceId
    this.deviceLabel = options.deviceLabel
    this.nostrSubscribe = options.nostrSubscribe
    this.nostrPublish = options.nostrPublish
    this.storage = options.storage || new InMemoryStorageAdapter()

    // Owner mode
    this.ownerPublicKey = options.ownerPublicKey
    this.identityKey = options.identityKey

    // Delegate mode
    this.devicePublicKey = options.devicePublicKey
    this.devicePrivateKey = options.devicePrivateKey
    this.ephemeralPublicKey = options.ephemeralPublicKey
    this.ephemeralPrivateKey = options.ephemeralPrivateKey
    this.sharedSecret = options.sharedSecret
  }

  /**
   * Create a DeviceManager with owner's nsec (can manage InviteList)
   * For extension login, pass { encrypt, decrypt } functions instead of raw private key.
   */
  static createOwnerDevice(options: OwnerDeviceOptions): DeviceManager {
    return new DeviceManager({
      delegateMode: false,
      deviceId: options.deviceId,
      deviceLabel: options.deviceLabel,
      nostrSubscribe: options.nostrSubscribe,
      nostrPublish: options.nostrPublish,
      storage: options.storage,
      ownerPublicKey: options.ownerPublicKey,
      identityKey: options.identityKey,
    })
  }

  /**
   * Create a DeviceManager for a delegate device (no nsec, generates own keys)
   */
  static createDelegate(options: DelegateDeviceOptions): CreateDelegateResult {
    // Generate identity keypair for this delegate device
    const devicePrivateKey = generateSecretKey()
    const devicePublicKey = getPublicKey(devicePrivateKey)

    // Generate ephemeral keypair for invite handshakes
    const ephemeralPrivateKey = generateSecretKey()
    const ephemeralPublicKey = getPublicKey(ephemeralPrivateKey)

    // Generate shared secret
    const sharedSecret = bytesToHex(generateSecretKey())

    const manager = new DeviceManager({
      delegateMode: true,
      deviceId: options.deviceId,
      deviceLabel: options.deviceLabel,
      nostrSubscribe: options.nostrSubscribe,
      nostrPublish: options.nostrPublish,
      storage: options.storage,
      devicePublicKey,
      devicePrivateKey,
      ephemeralPublicKey,
      ephemeralPrivateKey,
      sharedSecret,
    })

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
   * Restore a DeviceManager for a delegate device from existing credentials.
   * Use this when restoring a delegate device from a pairing code.
   */
  static restoreDelegate(options: RestoreDelegateOptions): DeviceManager {
    return new DeviceManager({
      delegateMode: true,
      deviceId: options.deviceId,
      deviceLabel: options.deviceLabel,
      nostrSubscribe: options.nostrSubscribe,
      nostrPublish: options.nostrPublish,
      storage: options.storage,
      devicePublicKey: options.devicePublicKey,
      devicePrivateKey: options.devicePrivateKey,
      ephemeralPublicKey: options.ephemeralPublicKey,
      ephemeralPrivateKey: options.ephemeralPrivateKey,
      sharedSecret: options.sharedSecret,
    })
  }

  /**
   * Initialize the DeviceManager
   */
  async init(): Promise<void> {
    if (this.initialized) return
    this.initialized = true

    if (this.delegateMode) {
      await this.initDelegateMode()
    } else {
      await this.initOwnerMode()
    }
  }

  private async initOwnerMode(): Promise<void> {
    if (!this.ownerPublicKey) {
      throw new Error("Owner public key required for owner mode")
    }

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

  private async initDelegateMode(): Promise<void> {
    // Load stored owner pubkey if exists
    const storedOwnerPubkey = await this.storage.get<string>(this.ownerPubkeyKey())
    if (storedOwnerPubkey) {
      this.ownerPubkeyFromActivation = storedOwnerPubkey
    }
  }

  /**
   * Returns whether this is a delegate device
   */
  isDelegateMode(): boolean {
    return this.delegateMode
  }

  /**
   * Get the device ID
   */
  getDeviceId(): string {
    return this.deviceId
  }

  /**
   * Get the device label
   */
  getDeviceLabel(): string {
    return this.deviceLabel
  }

  /**
   * Get the identity public key (owner pubkey for owner mode, device pubkey for delegate)
   */
  getIdentityPublicKey(): string {
    if (this.delegateMode) {
      if (!this.devicePublicKey) {
        throw new Error("Device public key not set")
      }
      return this.devicePublicKey
    }
    if (!this.ownerPublicKey) {
      throw new Error("Owner public key not set")
    }
    return this.ownerPublicKey
  }

  /**
   * Get the identity key (owner key for owner mode, device key for delegate)
   * Returns Uint8Array for raw key mode, or { encrypt, decrypt } for extension login mode.
   */
  getIdentityKey(): IdentityKey {
    if (this.delegateMode) {
      if (!this.devicePrivateKey) {
        throw new Error("Device private key not set")
      }
      return this.devicePrivateKey
    }
    if (!this.identityKey) {
      throw new Error("Identity key not set")
    }
    return this.identityKey
  }

  /**
   * Check if this DeviceManager is using extension login (function callbacks instead of raw keys)
   */
  isExtensionLogin(): boolean {
    return this.identityKey !== undefined && !(this.identityKey instanceof Uint8Array)
  }

  /**
   * Get the ephemeral keypair for invite handshakes
   */
  getEphemeralKeypair(): { publicKey: string; privateKey: Uint8Array } | null {
    if (this.delegateMode) {
      if (!this.ephemeralPublicKey || !this.ephemeralPrivateKey) {
        return null
      }
      return {
        publicKey: this.ephemeralPublicKey,
        privateKey: this.ephemeralPrivateKey,
      }
    }

    // For owner mode, get from InviteList
    const device = this.inviteList?.getDevice(this.deviceId)
    if (!device?.ephemeralPublicKey || !device?.ephemeralPrivateKey) {
      return null
    }
    return {
      publicKey: device.ephemeralPublicKey,
      privateKey: device.ephemeralPrivateKey,
    }
  }

  /**
   * Get the shared secret for invite handshakes
   */
  getSharedSecret(): string | null {
    if (this.delegateMode) {
      return this.sharedSecret || null
    }

    // For owner mode, get from InviteList
    const device = this.inviteList?.getDevice(this.deviceId)
    return device?.sharedSecret || null
  }

  /**
   * Get the InviteList (owner mode only)
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
   * Add a device to the InviteList (owner mode only)
   */
  async addDevice(payload: DevicePayload): Promise<void> {
    if (this.delegateMode) {
      throw new Error("Cannot add devices in delegate mode")
    }

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
   * Revoke a device from the InviteList (owner mode only)
   */
  async revokeDevice(deviceId: string): Promise<void> {
    if (this.delegateMode) {
      throw new Error("Cannot revoke devices in delegate mode")
    }

    if (deviceId === this.deviceId) {
      throw new Error("Cannot revoke own device")
    }

    await this.init()

    await this.modifyInviteList((list) => {
      list.removeDevice(deviceId)
    })
  }

  /**
   * Update a device's label (owner mode only)
   */
  async updateDeviceLabel(deviceId: string, label: string): Promise<void> {
    if (this.delegateMode) {
      throw new Error("Cannot update device labels in delegate mode")
    }

    await this.init()

    await this.modifyInviteList((list) => {
      list.updateDeviceLabel(deviceId, label)
    })
  }

  /**
   * Wait for this delegate device to be activated (added to an InviteList)
   * Returns the owner's public key
   */
  async waitForActivation(timeoutMs = 60000): Promise<string> {
    if (!this.delegateMode) {
      throw new Error("waitForActivation is only for delegate mode")
    }

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
   * Get the owner's public key (delegate mode, after activation)
   */
  getOwnerPublicKey(): string | null {
    if (!this.delegateMode) {
      return this.ownerPublicKey || null
    }
    return this.ownerPubkeyFromActivation || null
  }

  /**
   * Check if this delegate device has been revoked
   */
  async isRevoked(): Promise<boolean> {
    if (!this.delegateMode) {
      return false
    }

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

  /**
   * Clean up subscriptions
   */
  close(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe()
    }
    this.subscriptions = []
  }

  /**
   * Creates a SessionManager configured for this device.
   * Must be called after init().
   *
   * For owner mode: Uses owner's keys and ephemeral keys from InviteList
   * For delegate devices: Uses device's own identity keys and ephemeral keys
   *
   * @param sessionStorage - Optional separate storage for SessionManager (defaults to DeviceManager's storage)
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

    const publicKey = this.getIdentityPublicKey()
    const identityKey = this.getIdentityKey()
    const ownerPublicKey = this.getOwnerPublicKey()

    if (!ownerPublicKey) {
      throw new Error("Owner public key required for SessionManager")
    }

    return new SessionManager(
      publicKey,
      identityKey,
      this.deviceId,
      this.nostrSubscribe,
      this.nostrPublish,
      ownerPublicKey,
      sessionStorage || this.storage,
      ephemeralKeypair,
      sharedSecret,
    )
  }

  // Private helpers

  private inviteListKey(): string {
    return `${this.versionPrefix}/device-manager/invite-list`
  }

  private ownerPubkeyKey(): string {
    return `${this.versionPrefix}/device-manager/owner-pubkey`
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
    if (!this.ownerPublicKey) {
      throw new Error("Owner public key required")
    }
    if (local && remote) return local.merge(remote)
    if (local) return local
    if (remote) return remote
    return new InviteList(this.ownerPublicKey)
  }

  private async modifyInviteList(change: (list: InviteList) => void): Promise<void> {
    if (!this.ownerPublicKey) {
      throw new Error("Owner public key required")
    }

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
    if (!this.ownerPublicKey) return

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
