import { generateSecretKey, getPublicKey, VerifiedEvent } from "nostr-tools"
import { InviteList, DeviceEntry } from "./InviteList"
import { Invite } from "./Invite"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, Unsubscribe, IdentityKey } from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { SessionManager } from "./SessionManager"

/**
 * Payload for adding a delegate device to the owner's InviteList.
 * Contains only identity information - invite crypto is in separate Invite events.
 */
export interface DelegateDevicePayload {
  /** Device ID (16 hex chars) */
  deviceId: string
  /** Human-readable device label */
  deviceLabel: string
  /** Identity public key for this device (64 hex chars) */
  identityPubkey: string
}

export interface OwnerDeviceOptions {
  ownerPublicKey: string
  identityKey: IdentityKey
  deviceId: string
  deviceLabel: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

export interface DelegateDeviceOptions {
  deviceId: string
  deviceLabel: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

export interface RestoreDelegateOptions {
  deviceId: string
  devicePublicKey: string
  devicePrivateKey: Uint8Array
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

export interface CreateDelegateResult {
  manager: DelegateDeviceManager
  payload: DelegateDevicePayload
}

export interface IDeviceManager {
  init(): Promise<void>
  getDeviceId(): string
  getIdentityPublicKey(): string
  getIdentityKey(): IdentityKey
  getInvite(): Invite | null
  getOwnerPublicKey(): string | null
  close(): void
  createSessionManager(sessionStorage?: StorageAdapter): SessionManager
}

/** Owner's main device. Has identity key and can manage InviteList. */
export class OwnerDeviceManager implements IDeviceManager {
  private readonly deviceId: string
  private readonly deviceLabel: string
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter
  private readonly ownerPublicKey: string
  private readonly identityKey: IdentityKey

  private inviteList: InviteList | null = null
  private invite: Invite | null = null
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "2" // Bump for new invite architecture
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

    // Load or create Invite for this device
    const savedInvite = await this.loadInvite()
    this.invite = savedInvite || Invite.createNew(this.ownerPublicKey, this.deviceId)
    await this.saveInvite(this.invite)

    // Load and merge InviteList
    const local = await this.loadInviteList()
    const remote = await this.fetchInviteList(this.ownerPublicKey)
    const inviteList = this.mergeInviteLists(local, remote)

    // Add this device to InviteList if not present (only identity, no invite crypto)
    if (!inviteList.getDevice(this.deviceId)) {
      const device = inviteList.createDeviceEntry(
        this.deviceLabel,
        this.ownerPublicKey,
        this.deviceId
      )
      inviteList.addDevice(device)
    }

    this.inviteList = inviteList
    await this.saveInviteList(inviteList)

    // Publish both InviteList and device's Invite
    const inviteListEvent = inviteList.getEvent()
    await this.nostrPublish(inviteListEvent).catch((error) => {
      console.error("Failed to publish InviteList:", error)
    })

    const inviteEvent = this.invite.getEvent()
    await this.nostrPublish(inviteEvent).catch((error) => {
      console.error("Failed to publish Invite:", error)
    })

    this.subscribeToOwnInviteList()
  }

  getDeviceId(): string {
    return this.deviceId
  }

  getIdentityPublicKey(): string {
    return this.ownerPublicKey
  }

  getIdentityKey(): IdentityKey {
    return this.identityKey
  }

  getInvite(): Invite | null {
    return this.invite
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
   * Add a delegate device to the InviteList.
   * Only adds identity info - the delegate device publishes its own Invite separately.
   */
  async addDevice(payload: DelegateDevicePayload): Promise<void> {
    await this.init()

    await this.modifyInviteList((list) => {
      const device: DeviceEntry = {
        deviceId: payload.deviceId,
        deviceLabel: payload.deviceLabel,
        createdAt: Math.floor(Date.now() / 1000),
        identityPubkey: payload.identityPubkey,
      }
      list.addDevice(device)
    })
  }

  /**
   * Rotate this device's invite - generates new ephemeral keys and shared secret.
   */
  async rotateInvite(): Promise<void> {
    await this.init()

    this.invite = Invite.createNew(this.ownerPublicKey, this.deviceId)
    await this.saveInvite(this.invite)

    const inviteEvent = this.invite.getEvent()
    await this.nostrPublish(inviteEvent)
  }

  async revokeDevice(deviceId: string): Promise<void> {
    if (deviceId === this.deviceId) {
      throw new Error("Cannot revoke own device")
    }

    await this.init()

    await this.modifyInviteList((list) => {
      list.removeDevice(deviceId)
    })
  }

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

  createSessionManager(sessionStorage?: StorageAdapter): SessionManager {
    if (!this.initialized) {
      throw new Error("DeviceManager must be initialized before creating SessionManager")
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

  private inviteListKey(): string {
    return `${this.versionPrefix}/device-manager/invite-list`
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
            if (!latestEvent || event.created_at >= latestEvent.event.created_at) {
              latestEvent = { event, inviteList }
            }
          } catch {
            // Invalid event
          }
        }
      )

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
    const remote = await this.fetchInviteList(this.ownerPublicKey)
    const merged = this.mergeInviteLists(this.inviteList, remote)
    change(merged)

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

/** Delegate device. Has own identity key, waits for activation, checks revocation. */
export class DelegateDeviceManager implements IDeviceManager {
  private readonly deviceId: string
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter

  private readonly devicePublicKey: string
  private readonly devicePrivateKey: Uint8Array

  private invite: Invite | null = null
  private ownerPubkeyFromActivation?: string
  private initialized = false
  private subscriptions: Unsubscribe[] = []

  private readonly storageVersion = "2" // Bump for new invite architecture
  private get versionPrefix(): string {
    return `v${this.storageVersion}`
  }

  private constructor(
    deviceId: string,
    nostrSubscribe: NostrSubscribe,
    nostrPublish: NostrPublish,
    storage: StorageAdapter,
    devicePublicKey: string,
    devicePrivateKey: Uint8Array,
  ) {
    this.deviceId = deviceId
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.storage = storage
    this.devicePublicKey = devicePublicKey
    this.devicePrivateKey = devicePrivateKey
  }

  static create(options: DelegateDeviceOptions): CreateDelegateResult {
    const devicePrivateKey = generateSecretKey()
    const devicePublicKey = getPublicKey(devicePrivateKey)

    const manager = new DelegateDeviceManager(
      options.deviceId,
      options.nostrSubscribe,
      options.nostrPublish,
      options.storage || new InMemoryStorageAdapter(),
      devicePublicKey,
      devicePrivateKey,
    )

    // Payload only contains identity info - invite crypto is created/published separately
    const payload: DelegateDevicePayload = {
      deviceId: options.deviceId,
      deviceLabel: options.deviceLabel,
      identityPubkey: devicePublicKey,
    }

    return { manager, payload }
  }

  static restore(options: RestoreDelegateOptions): DelegateDeviceManager {
    return new DelegateDeviceManager(
      options.deviceId,
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
    this.invite = savedInvite || Invite.createNew(this.devicePublicKey, this.deviceId)
    await this.saveInvite(this.invite)

    // Publish Invite event (signed by this device's identity key)
    const inviteEvent = this.invite.getEvent()
    await this.nostrPublish(inviteEvent).catch((error) => {
      console.error("Failed to publish Invite:", error)
    })
  }

  getDeviceId(): string {
    return this.deviceId
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

    this.invite = Invite.createNew(this.devicePublicKey, this.deviceId)
    await this.saveInvite(this.invite)

    const inviteEvent = this.invite.getEvent()
    await this.nostrPublish(inviteEvent)
  }

  async waitForActivation(timeoutMs = 60000): Promise<string> {
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

            // Check that our identity pubkey matches
            if (device && device.identityPubkey === this.devicePublicKey) {
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

  async isRevoked(): Promise<boolean> {
    const ownerPubkey = this.getOwnerPublicKey()
    if (!ownerPubkey) return false

    const inviteList = await this.fetchInviteList(ownerPubkey)
    if (!inviteList) return true

    const device = inviteList.getDevice(this.deviceId)
    // Device is revoked if not in list or identity doesn't match
    return !device || device.identityPubkey !== this.devicePublicKey
  }

  close(): void {
    for (const unsubscribe of this.subscriptions) {
      unsubscribe()
    }
    this.subscriptions = []
  }

  createSessionManager(sessionStorage?: StorageAdapter): SessionManager {
    if (!this.initialized) {
      throw new Error("DeviceManager must be initialized before creating SessionManager")
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
      this.deviceId,
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
            // Invalid event
          }
        }
      )

      if (resolved) {
        unsubscribe()
      }
    })
  }
}
