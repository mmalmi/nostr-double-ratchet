import { generateSecretKey, getPublicKey, VerifiedEvent } from "nostr-tools"
import { bytesToHex } from "@noble/hashes/utils"
import { InviteList, DeviceEntry } from "./InviteList"
import { DevicePayload } from "./inviteUtils"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, Unsubscribe, IdentityKey } from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { SessionManager } from "./SessionManager"

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
  ephemeralPublicKey: string
  ephemeralPrivateKey: Uint8Array
  sharedSecret: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

export interface CreateDelegateResult {
  manager: DelegateDeviceManager
  payload: DevicePayload
}

export interface IDeviceManager {
  init(): Promise<void>
  getDeviceId(): string
  getIdentityPublicKey(): string
  getIdentityKey(): IdentityKey
  getEphemeralKeypair(): { publicKey: string; privateKey: Uint8Array } | null
  getSharedSecret(): string | null
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

    const local = await this.loadInviteList()
    const remote = await this.fetchInviteList(this.ownerPublicKey)
    const inviteList = this.mergeInviteLists(local, remote)

    if (!inviteList.getDevice(this.deviceId)) {
      const device = inviteList.createDevice(this.deviceLabel, this.deviceId)
      inviteList.addDevice(device)
    }

    this.inviteList = inviteList
    await this.saveInviteList(inviteList)

    const event = inviteList.getEvent()
    await this.nostrPublish(event).catch((error) => {
      console.error("Failed to publish InviteList:", error)
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

  getInviteList(): InviteList | null {
    return this.inviteList
  }

  getOwnDevices(): DeviceEntry[] {
    return this.inviteList?.getAllDevices() || []
  }

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
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.storage = storage
    this.devicePublicKey = devicePublicKey
    this.devicePrivateKey = devicePrivateKey
    this.ephemeralPublicKey = ephemeralPublicKey
    this.ephemeralPrivateKey = ephemeralPrivateKey
    this.sharedSecret = sharedSecret
  }

  static create(options: DelegateDeviceOptions): CreateDelegateResult {
    const devicePrivateKey = generateSecretKey()
    const devicePublicKey = getPublicKey(devicePrivateKey)
    const ephemeralPrivateKey = generateSecretKey()
    const ephemeralPublicKey = getPublicKey(ephemeralPrivateKey)
    const sharedSecret = bytesToHex(generateSecretKey())

    const manager = new DelegateDeviceManager(
      options.deviceId,
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

  static restore(options: RestoreDelegateOptions): DelegateDeviceManager {
    return new DelegateDeviceManager(
      options.deviceId,
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

    const storedOwnerPubkey = await this.storage.get<string>(this.ownerPubkeyKey())
    if (storedOwnerPubkey) {
      this.ownerPubkeyFromActivation = storedOwnerPubkey
    }
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

            if (device && device.ephemeralPublicKey === this.ephemeralPublicKey) {
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
    return !device || device.ephemeralPublicKey !== this.ephemeralPublicKey
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
