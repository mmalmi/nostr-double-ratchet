import {
  IdentityKey,
  NostrSubscribe,
  NostrPublish,
  Rumor,
  Unsubscribe,
  APP_KEYS_EVENT_KIND,
  CHAT_MESSAGE_KIND,
  CHAT_SETTINGS_KIND,
  RECEIPT_KIND,
  TYPING_KIND,
  ReceiptType,
  ExpirationOptions,
  ChatSettingsPayloadV1,
} from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { MessageQueue } from "./MessageQueue"
import { AppKeys } from "./AppKeys"
import { Invite } from "./Invite"
import { Session } from "./Session"
import { GROUP_METADATA_KIND } from "./GroupMeta"
import {
  deserializeSessionState,
  resolveExpirationSeconds,
  serializeSessionState,
  upsertExpirationTag,
} from "./utils"
import { decryptInviteResponse, createSessionFromAccept } from "./inviteUtils"
import { getEventHash } from "nostr-tools"
import {
  classifyMessageOrigin,
  isCrossDeviceSelfOrigin,
  isSelfOrigin,
} from "./MessageOrigin"
import { DeviceRecordActor } from "./session-manager/DeviceRecordActor"
import { UserRecordActor } from "./session-manager/UserRecordActor"
import type {
  AcceptInviteOptions,
  AcceptInviteResult,
  DeviceRecord,
  InviteCredentials,
  NostrFacade,
  OnEventCallback,
  OnEventMeta,
  StoredSessionEntry,
  StoredUserRecord,
  UserRecord,
} from "./session-manager/types"

export type {
  AcceptInviteOptions,
  AcceptInviteResult,
  DeviceRecord,
  InviteCredentials,
  OnEventCallback,
  OnEventMeta,
  UserRecord,
} from "./session-manager/types"

export class SessionManager {
  // Versioning
  private readonly storageVersion = "1"
  private readonly versionPrefix: string

  // Params
  private deviceId: string
  private storage: StorageAdapter
  private nostrSubscribe: NostrSubscribe
  private nostrPublish: NostrPublish
  private identityKey: IdentityKey
  private ourPublicKey: string
  // Owner's public key - used for grouping devices together (all devices are delegates)
  private ownerPublicKey: string
  private nostrFacade: NostrFacade

  // Credentials for invite handshake
  private inviteKeys: InviteCredentials

  // Data
  private userRecords: Map<string, UserRecordActor> = new Map()
  private messageQueue!: MessageQueue
  private discoveryQueue!: MessageQueue
  // Map delegate device pubkeys to their owner's pubkey
  private delegateToOwner: Map<string, string> = new Map()
  // Track processed InviteResponse event IDs to prevent replay
  private processedInviteResponses: Set<string> = new Set()
  // Expiration defaults (persisted)
  private defaultExpiration: ExpirationOptions | undefined
  private peerExpiration: Map<string, ExpirationOptions | null> = new Map()
  private groupExpiration: Map<string, ExpirationOptions | null> = new Map()
  private autoAdoptChatSettings: boolean = true

  // Persist user records in-order per key so older async writes can't overwrite newer state.
  private userRecordWriteChain: Map<string, Promise<void>> = new Map()

  // Subscriptions
  private ourInviteResponseSubscription: Unsubscribe | null = null

  // Callbacks
  private internalSubscriptions: Set<OnEventCallback> = new Set()

  // Initialization flag
  private initialized: boolean = false

  private expirationDefaultKey(): string {
    return `${this.versionPrefix}/expiration/default`
  }

  private expirationPeerPrefix(): string {
    return `${this.versionPrefix}/expiration/peer/`
  }

  private expirationPeerKey(peerPubkey: string): string {
    return `${this.expirationPeerPrefix()}${peerPubkey}`
  }

  private expirationGroupPrefix(): string {
    return `${this.versionPrefix}/expiration/group/`
  }

  private expirationGroupKey(groupId: string): string {
    return `${this.expirationGroupPrefix()}${encodeURIComponent(groupId)}`
  }

  private validateExpirationOptions(options: ExpirationOptions | undefined): void {
    if (!options) return
    // Validates mutual exclusivity + integer seconds.
    resolveExpirationSeconds(options, 0)
  }

  private async loadExpirationSettings(): Promise<void> {
    // Default
    const def = await this.storage.get<ExpirationOptions>(this.expirationDefaultKey())
    if (def) {
      try {
        this.validateExpirationOptions(def)
        this.defaultExpiration = def
      } catch {
        // Ignore invalid stored values
      }
    }

    // Per-peer
    const peerKeys = await this.storage.list(this.expirationPeerPrefix())
    for (const k of peerKeys) {
      const peer = k.slice(this.expirationPeerPrefix().length)
      if (!peer) continue
      const v = await this.storage.get<ExpirationOptions | null>(k)
      if (v === undefined) continue
      if (v === null) {
        this.peerExpiration.set(peer, null)
        continue
      }
      try {
        this.validateExpirationOptions(v)
        this.peerExpiration.set(peer, v)
      } catch {
        // Ignore invalid stored values
      }
    }

    // Per-group
    const groupKeys = await this.storage.list(this.expirationGroupPrefix())
    for (const k of groupKeys) {
      const enc = k.slice(this.expirationGroupPrefix().length)
      if (!enc) continue
      let groupId: string
      try {
        groupId = decodeURIComponent(enc)
      } catch {
        continue
      }
      const v = await this.storage.get<ExpirationOptions | null>(k)
      if (v === undefined) continue
      if (v === null) {
        this.groupExpiration.set(groupId, null)
        continue
      }
      try {
        this.validateExpirationOptions(v)
        this.groupExpiration.set(groupId, v)
      } catch {
        // Ignore invalid stored values
      }
    }
  }

  constructor(
    ourPublicKey: string,
    identityKey: IdentityKey,
    deviceId: string,
    nostrSubscribe: NostrSubscribe,
    nostrPublish: NostrPublish,
    ownerPublicKey: string,
    inviteKeys: InviteCredentials,
    storage?: StorageAdapter,
  ) {
    this.userRecords = new Map()
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.ourPublicKey = ourPublicKey
    this.identityKey = identityKey
    this.deviceId = deviceId
    this.ownerPublicKey = ownerPublicKey
    this.inviteKeys = inviteKeys
    this.storage = storage || new InMemoryStorageAdapter()
    this.versionPrefix = `v${this.storageVersion}`
    this.messageQueue = new MessageQueue(this.storage, "v1/message-queue/")
    this.discoveryQueue = new MessageQueue(this.storage, "v1/discovery-queue/")
    this.nostrFacade = {
      subscribe: this.nostrSubscribe,
      publish: async (event) => {
        await this.nostrPublish(event)
      },
    }
  }

  async init() {
    if (this.initialized) return
    this.initialized = true

    await this.runMigrations().catch(() => {
      // Failed to run migrations
    })

    await this.loadAllUserRecords().catch(() => {
      // Failed to load user records
    })

    await this.loadExpirationSettings().catch(() => {
      // Failed to load expiration settings
    })

    // Add our own device to user record to prevent accepting our own invite
    // Use ownerPublicKey so delegates are added to the owner's record
    const ourUserRecord = this.getOrCreateUserRecord(this.ownerPublicKey)
    this.upsertDeviceRecord(ourUserRecord, this.deviceId)

    // Start invite response listener BEFORE setting up users
    // This ensures we're listening when other devices respond to our invites
    this.startInviteResponseListener()

    // Setup sessions with our own devices and resume discovery for all known users
    Array.from(this.userRecords.keys()).forEach(pubkey => this.setupUser(pubkey))
  }

  /**
   * Start listening for invite responses on our ephemeral key.
   * This is used by devices to receive session establishment responses.
   */
  private startInviteResponseListener(): void {
    const { publicKey: ephemeralPubkey, privateKey: ephemeralPrivkey } = this.inviteKeys.ephemeralKeypair
    const sharedSecret = this.inviteKeys.sharedSecret

    // Subscribe to invite responses tagged to our ephemeral key
    this.ourInviteResponseSubscription = this.nostrSubscribe(
      {
        kinds: [1059], // INVITE_RESPONSE_KIND
        "#p": [ephemeralPubkey],
      },
      async (event) => {
        // Skip already processed InviteResponses (prevents replay issues on restart)
        if (this.processedInviteResponses.has(event.id)) {
          return
        }
        this.processedInviteResponses.add(event.id)

        try {
          const decrypted = await decryptInviteResponse({
            envelopeContent: event.content,
            envelopeSenderPubkey: event.pubkey,
            inviterEphemeralPrivateKey: ephemeralPrivkey,
            inviterPrivateKey: this.identityKey instanceof Uint8Array ? this.identityKey : undefined,
            sharedSecret,
            decrypt: this.identityKey instanceof Uint8Array ? undefined : this.identityKey.decrypt,
          })

          // Skip our own responses - this happens when we publish an invite response
          // and our own listener receives it back from relays
          // inviteeIdentity serves as the device ID
          if (decrypted.inviteeIdentity === this.deviceId) {
            return
          }

          // Get owner pubkey from response (required for proper chat routing)
          // If not present (old client), fall back to resolveToOwner
          const claimedOwner = decrypted.ownerPublicKey || this.resolveToOwner(decrypted.inviteeIdentity)

          // Verify the device is authorized by fetching owner's AppKeys
          const appKeys = await this.fetchAppKeys(claimedOwner)

          if (appKeys) {
            const deviceInList = appKeys.getAllDevices().some(
              d => d.identityPubkey === decrypted.inviteeIdentity
            )
            if (!deviceInList) {
              return
            }
            this.updateDelegateMapping(claimedOwner, appKeys)
          } else {
            // No AppKeys from relay - check persisted AppKeys or single-device case
            const persistedAppKeys = this.userRecords.get(claimedOwner)?.appKeys
            const isCached = persistedAppKeys?.getAllDevices().some(
              d => d.identityPubkey === decrypted.inviteeIdentity
            ) ?? false
            const isSingleDevice = decrypted.inviteeIdentity === claimedOwner
            if (!isCached && !isSingleDevice) {
              return
            }
          }

          const ownerPubkey = claimedOwner
          const userRecord = this.getOrCreateUserRecord(ownerPubkey)
          // inviteeIdentity serves as the device ID
          const deviceRecord = this.upsertDeviceRecord(userRecord, decrypted.inviteeIdentity)

          const session = createSessionFromAccept({
            nostrSubscribe: this.nostrSubscribe,
            theirPublicKey: decrypted.inviteeSessionPublicKey,
            ourSessionPrivateKey: ephemeralPrivkey,
            sharedSecret,
            isSender: false,
            name: event.id,
          })

          deviceRecord.installSession(session, true)
          this.storeUserRecord(ownerPubkey).catch(() => {})
        } catch {
        }
      }
    )
  }

  /**
   * Fetch a user's AppKeys from relays.
   * Returns null if not found within timeout.
   */
  private fetchAppKeys(pubkey: string, timeoutMs = 2000): Promise<AppKeys | null> {
    return new Promise((resolve) => {
      let latestEvent: { created_at: number; appKeys: AppKeys } | null = null
      let resolved = false

      // Use a short initial delay before resolving to allow event delivery
      const resolveResult = () => {
        if (resolved) return
        resolved = true
        unsubscribe()
        resolve(latestEvent?.appKeys ?? null)
      }

      // Start timeout
      const timeout = setTimeout(resolveResult, timeoutMs)

      const unsubscribe = this.nostrSubscribe(
        {
          kinds: [APP_KEYS_EVENT_KIND],
          authors: [pubkey],
          "#d": ["double-ratchet/app-keys"],
        },
        (event) => {
          if (resolved) return
          try {
            const appKeys = AppKeys.fromEvent(event)
            // Use >= to prefer later-delivered events when timestamps are equal
            // This handles replaceable events created within the same second
            if (!latestEvent || event.created_at >= latestEvent.created_at) {
              latestEvent = { created_at: event.created_at, appKeys }
            }
            // Resolve quickly after receiving an event (allow for more events to arrive)
            clearTimeout(timeout)
            setTimeout(resolveResult, 100) // Short delay to collect any late events
          } catch {
            // Invalid event, ignore
          }
        }
      )
    })
  }

  // -------------------
  // User and Device Records helpers
  // -------------------
  private getOrCreateUserRecord(userPubkey: string): UserRecordActor {
    let rec = this.userRecords.get(userPubkey)
    if (!rec) {
      rec = new UserRecordActor(userPubkey, {
        manager: {
          updateDelegateMapping: (ownerPubkey, appKeys) => {
            this.updateDelegateMapping(ownerPubkey, appKeys)
          },
          removeDelegateMapping: (deviceId) => {
            this.delegateToOwner.delete(deviceId)
          },
          handleDeviceRumor: (ownerPubkey, deviceId, rumor) => {
            this.handleDeviceRumor(ownerPubkey, deviceId, rumor)
          },
          persistUserRecord: (ownerPubkey) => {
            this.storeUserRecord(ownerPubkey).catch(() => {})
          },
        },
        nostr: this.nostrFacade,
        messageQueue: this.messageQueue,
        discoveryQueue: this.discoveryQueue,
        ourDeviceId: this.deviceId,
        ourOwnerPubkey: this.ownerPublicKey,
        identityKey: this.identityKey,
      })
      this.userRecords.set(userPubkey, rec)
    }
    return rec
  }

  private handleDeviceRumor(ownerPubkey: string, deviceId: string, event: Rumor): void {
    this.maybeAutoAdoptChatSettings(event, ownerPubkey)

    const origin = classifyMessageOrigin({
      ourOwnerPubkey: this.ownerPublicKey,
      ourDevicePubkey: this.deviceId,
      senderOwnerPubkey: ownerPubkey,
      senderDevicePubkey: deviceId,
    })

    const meta: OnEventMeta = {
      fromDeviceId: deviceId,
      senderOwnerPubkey: ownerPubkey,
      senderDevicePubkey: deviceId,
      origin,
      isSelf: isSelfOrigin(origin),
      isCrossDeviceSelf: isCrossDeviceSelfOrigin(origin),
    }

    for (const cb of this.internalSubscriptions) {
      cb(event, ownerPubkey, meta)
    }
  }

  private upsertDeviceRecord(userRecord: UserRecordActor, deviceId: string): DeviceRecordActor {
    return userRecord.ensureDevice(deviceId)
  }

  private sessionKeyPrefix(userPubkey: string) {
    return `${this.versionPrefix}/session/${userPubkey}/`
  }

  private userRecordKey(publicKey: string) {
    return `${this.userRecordKeyPrefix()}${publicKey}`
  }

  private userRecordKeyPrefix() {
    return `${this.versionPrefix}/user/`
  }
  private versionKey() {
    return `storage-version`
  }

  /**
   * Resolve a pubkey to its owner if it's a known delegate device.
   * Returns the input pubkey if not a known delegate.
   */
  private resolveToOwner(pubkey: string): string {
    return this.delegateToOwner.get(pubkey) || pubkey
  }

  /**
   * Update the delegate-to-owner mapping from an AppKeys.
   * Extracts delegate device pubkeys and maps them to the owner.
   * Persists the mapping in the user record for restart recovery.
   */
  private updateDelegateMapping(ownerPubkey: string, appKeys: AppKeys): void {
    const userRecord = this.getOrCreateUserRecord(ownerPubkey)
    const newDeviceIdentities = new Set(
      appKeys.getAllDevices()
        .map(d => d.identityPubkey)
        .filter(Boolean) as string[]
    )

    // Remove stale mappings for devices no longer in AppKeys
    const oldIdentities = (userRecord.appKeys?.getAllDevices() || [])
      .map(d => d.identityPubkey)
      .filter(Boolean) as string[]
    for (const identity of oldIdentities) {
      if (!newDeviceIdentities.has(identity)) {
        this.delegateToOwner.delete(identity)
        this.messageQueue.removeForTarget(identity).catch(() => {})
      }
    }

    // Store AppKeys in user record (single source of truth)
    userRecord.appKeys = appKeys

    // Update in-memory mapping for current devices
    for (const identity of newDeviceIdentities) {
      this.delegateToOwner.set(identity, ownerPubkey)
    }

    // Persist
    this.storeUserRecord(ownerPubkey).catch(() => {})
  }

  /**
   * Check if a device is currently authorized by the owner's AppKeys.
   * Returns true if the device is in the owner's current AppKeys.
   */
  private isDeviceAuthorized(ownerPubkey: string, deviceId: string): boolean {
    const appKeys = this.userRecords.get(ownerPubkey)?.appKeys
    if (!appKeys) return false
    return appKeys.getAllDevices().some(d => d.identityPubkey === deviceId)
  }

  setupUser(userPubkey: string) {
    this.getOrCreateUserRecord(userPubkey).ensureSetup().catch(() => {})
  }

  onEvent(callback: OnEventCallback) {
    this.internalSubscriptions.add(callback)

    return () => {
      this.internalSubscriptions.delete(callback)
    }
  }

  /**
   * Enable/disable automatically adopting incoming `chat-settings` events (kind 10448).
   * When enabled, receiving a valid settings payload updates per-peer expiration defaults.
   */
  setAutoAdoptChatSettings(enabled: boolean) {
    this.autoAdoptChatSettings = enabled
  }

  getDeviceId(): string {
    return this.deviceId
  }

  getUserRecords(): Map<string, UserRecord> {
    return this.userRecords as unknown as Map<string, UserRecord>
  }

  /**
   * Set a global default expiration for outgoing rumors sent via this SessionManager.
   * Pass `undefined` to clear.
   */
  async setDefaultExpiration(options: ExpirationOptions | undefined): Promise<void> {
    this.validateExpirationOptions(options)
    this.defaultExpiration = options
    const key = this.expirationDefaultKey()
    if (!options) {
      await this.storage.del(key).catch(() => {})
      return
    }
    await this.storage.put(key, options).catch(() => {})
  }

  /**
   * Set a per-peer default expiration for outgoing rumors. Pass `undefined` to clear.
   */
  async setExpirationForPeer(
    peerPubkey: string,
    options: ExpirationOptions | null | undefined
  ): Promise<void> {
    this.validateExpirationOptions(options || undefined)
    if (options === undefined) {
      this.peerExpiration.delete(peerPubkey)
      await this.storage.del(this.expirationPeerKey(peerPubkey)).catch(() => {})
      return
    }
    this.peerExpiration.set(peerPubkey, options)
    await this.storage.put(this.expirationPeerKey(peerPubkey), options).catch(() => {})
  }

  /**
   * Set a per-group default expiration, keyed by groupId (typically carried via `["l", groupId]`).
   * Pass `undefined` to clear.
   */
  async setExpirationForGroup(
    groupId: string,
    options: ExpirationOptions | null | undefined
  ): Promise<void> {
    this.validateExpirationOptions(options || undefined)
    if (options === undefined) {
      this.groupExpiration.delete(groupId)
      await this.storage.del(this.expirationGroupKey(groupId)).catch(() => {})
      return
    }
    this.groupExpiration.set(groupId, options)
    await this.storage.put(this.expirationGroupKey(groupId), options).catch(() => {})
  }

  close() {
    for (const userRecord of this.userRecords.values()) {
      userRecord.close()
    }

    this.ourInviteResponseSubscription?.()
  }

  deactivateCurrentSessions(publicKey: string) {
    const userRecord = this.userRecords.get(publicKey)
    if (!userRecord) return
    userRecord.deactivateCurrentSessions()
    this.storeUserRecord(publicKey).catch(() => {})
  }

  async deleteChat(userPubkey: string): Promise<void> {
    return this.deleteUser(this.resolveToOwner(userPubkey))
  }

  async deleteUser(userPubkey: string): Promise<void> {
    await this.init()

    const ownerPubkey = this.resolveToOwner(userPubkey)
    if (ownerPubkey === this.ownerPublicKey) return

    const userRecord = this.userRecords.get(ownerPubkey)

    if (userRecord) {
      userRecord.close()
      for (const device of userRecord.devices.values()) {
        await device.revoke()
      }
      this.userRecords.delete(ownerPubkey)
    }

    // Remove discovery queue entries for this owner
    await this.discoveryQueue.removeForTarget(ownerPubkey)
    // Remove message queue entries for all known devices
    if (userRecord) {
      for (const [deviceId] of userRecord.devices) {
        await this.messageQueue.removeForTarget(deviceId)
      }
    }

    await Promise.allSettled([
      this.deleteUserSessionsFromStorage(ownerPubkey),
      this.storage.del(this.userRecordKey(ownerPubkey)),
    ])
  }

  private async deleteUserSessionsFromStorage(userPubkey: string): Promise<void> {
    const prefix = this.sessionKeyPrefix(userPubkey)
    const keys = await this.storage.list(prefix)
    await Promise.all(keys.map((key) => this.storage.del(key)))
  }

  private async flushMessageQueue(deviceIdentity: string): Promise<void> {
    const ownerPubkey = this.resolveToOwner(deviceIdentity)
    const userRecord = this.userRecords.get(ownerPubkey)
    const device = userRecord?.devices.get(deviceIdentity)
    if (!device) {
      return
    }

    await device.flushMessageQueue()
    await this.storeUserRecord(ownerPubkey).catch(() => {})
  }

  async acceptInvite(
    invite: Invite,
    options: AcceptInviteOptions = {}
  ): Promise<AcceptInviteResult> {
    await this.init()

    const deviceId = invite.deviceId || invite.inviter
    if (!deviceId) {
      throw new Error("Invite device id is required")
    }

    if (deviceId === this.deviceId) {
      throw new Error("Cannot accept invite from this device")
    }

    const claimedOwnerPublicKey =
      options.ownerPublicKey ||
      invite.ownerPubkey ||
      this.resolveToOwner(deviceId) ||
      deviceId

    let ownerPublicKey = claimedOwnerPublicKey
    let preloadedAppKeys: AppKeys | null = null

    // When an invite claims delegate ownership, verify against AppKeys when available.
    // If claim verification fails for chat invites, fall back to device-identity routing.
    // For owner-side link flow, allow pre-registration acceptance and register via AppKeys afterward.
    if (claimedOwnerPublicKey !== deviceId) {
      const appKeys = await this.fetchAppKeys(claimedOwnerPublicKey).catch(() => null)
      if (appKeys) {
        const isAuthorized = appKeys
          .getAllDevices()
          .some((device) => device.identityPubkey === deviceId)
        if (isAuthorized) {
          preloadedAppKeys = appKeys
          this.updateDelegateMapping(claimedOwnerPublicKey, appKeys)
        } else if (!(invite.purpose === "link" && claimedOwnerPublicKey === this.ownerPublicKey)) {
          ownerPublicKey = deviceId
        }
      } else {
        const persistedAppKeys = this.userRecords.get(claimedOwnerPublicKey)?.appKeys
        const isAuthorized =
          persistedAppKeys
            ?.getAllDevices()
            .some((device) => device.identityPubkey === deviceId) ?? false
        if (
          persistedAppKeys &&
          !isAuthorized &&
          !(invite.purpose === "link" && claimedOwnerPublicKey === this.ownerPublicKey)
        ) {
          ownerPublicKey = deviceId
        }
      }
    }

    const userRecord = this.getOrCreateUserRecord(ownerPublicKey)
    if (preloadedAppKeys && ownerPublicKey === claimedOwnerPublicKey) {
      userRecord.appKeys = preloadedAppKeys
    }

    const existingRecord = userRecord.devices.get(deviceId)
    if (existingRecord?.activeSession) {
      return { ownerPublicKey, deviceId, session: existingRecord.activeSession }
    }

    const encryptor =
      this.identityKey instanceof Uint8Array ? this.identityKey : this.identityKey.encrypt
    const inviteeOwnerClaim = await this.resolveInviteeOwnerClaim()
    const { session, event } = await invite.accept(
      this.nostrSubscribe,
      this.ourPublicKey,
      encryptor,
      inviteeOwnerClaim
    )
    await this.nostrPublish(event)

    const deviceRecord = this.upsertDeviceRecord(userRecord, deviceId)
    this.delegateToOwner.set(deviceId, ownerPublicKey)
    deviceRecord.installSession(session)
    await this.flushMessageQueue(deviceId).catch(() => {})
    await this.storeUserRecord(ownerPublicKey).catch(() => {})

    return { ownerPublicKey, deviceId, session }
  }

  private async resolveInviteeOwnerClaim(): Promise<string | undefined> {
    if (this.deviceId === this.ownerPublicKey) {
      return this.ownerPublicKey
    }

    if (this.isDeviceAuthorized(this.ownerPublicKey, this.deviceId)) {
      return this.ownerPublicKey
    }

    const fetchedAppKeys = await this.fetchAppKeys(this.ownerPublicKey, 1000).catch(() => null)
    if (!fetchedAppKeys) {
      return undefined
    }

    this.updateDelegateMapping(this.ownerPublicKey, fetchedAppKeys)

    return fetchedAppKeys
      .getAllDevices()
      .some((device) => device.identityPubkey === this.deviceId)
      ? this.ownerPublicKey
      : undefined
  }

  async sendEvent(
    recipientIdentityKey: string,
    event: Partial<Rumor>
  ): Promise<Rumor | undefined> {
    await this.init()

    // Queue event for devices that don't have sessions yet
    const completeEvent = event as Rumor
    const targets = new Set([recipientIdentityKey, this.ownerPublicKey])
    for (const target of targets) {
      const userRecord = this.userRecords.get(target)
      const devices = userRecord?.appKeys?.getAllDevices() ?? []

      const otherDevices = devices.filter(
        d => d.identityPubkey && d.identityPubkey !== this.deviceId
      )

      if (otherDevices.length > 0) {
        // AppKeys known: queue per-device, skip discovery to avoid growth
        for (const device of otherDevices) {
          await this.messageQueue.add(device.identityPubkey, completeEvent)
        }
      } else {
        await this.discoveryQueue.add(target, completeEvent)
      }
    }

    const userRecord = this.getOrCreateUserRecord(recipientIdentityKey)
    // Use ownerPublicKey to find sibling devices (important for delegates)
    const ourUserRecord = this.getOrCreateUserRecord(this.ownerPublicKey)

    this.setupUser(recipientIdentityKey)
    // Use ownerPublicKey to setup sessions with sibling devices
    this.setupUser(this.ownerPublicKey)

    const recipientDevices = Array.from(userRecord.devices.values())
    const ownDevices = Array.from(ourUserRecord.devices.values())

    // Merge and deduplicate by deviceId, excluding our own sending device
    // This fixes the self-message bug where sending to yourself would duplicate devices
    const deviceMap = new Map<string, DeviceRecord>()
    for (const d of [...recipientDevices, ...ownDevices]) {
      if (d.deviceId !== this.deviceId) {  // Exclude sender's own device
        deviceMap.set(d.deviceId, d)
      }
    }
    const devices = Array.from(deviceMap.values())

    // Ratchet all sessions synchronously first, then persist state BEFORE network I/O.
    //
    // This is important for apps that "fire and forget" sendEvent() (e.g. UI click handlers):
    // if the page reloads/crashes while publishes are still in-flight, we still want the
    // updated session keys to be on disk so the next incoming message can be decrypted.
    const toPublish: Parameters<NostrPublish>[0][] = []
    const sentDeviceIds: string[] = []
    for (const device of devices) {
      const { activeSession } = device
      if (!activeSession) continue

      // Check if device is still authorized
      const deviceOwner = this.resolveToOwner(device.deviceId)
      if (deviceOwner !== device.deviceId && !this.isDeviceAuthorized(deviceOwner, device.deviceId)) {
        continue
      }

      try {
        const { event: verifiedEvent } = activeSession.sendEvent(event)
        toPublish.push(verifiedEvent)
        sentDeviceIds.push(device.deviceId)
      } catch {
        // Ignore send errors for a single device.
      }
    }

    // Persist recipient + owner records before publishing (best-effort).
    await this.storeUserRecord(recipientIdentityKey).catch(() => {})
    if (this.ownerPublicKey !== recipientIdentityKey) {
      await this.storeUserRecord(this.ownerPublicKey).catch(() => {})
    }

    await Promise.allSettled(
      toPublish.map((evt, i) =>
        this.nostrPublish(evt).then(() => {
          const deviceId = sentDeviceIds[i]
          this.messageQueue.removeByTargetAndEventId(deviceId, (event as Rumor).id).catch(() => {})
          this.flushMessageQueue(deviceId).catch(() => {})
        })
      )
    )

    // Return the event with computed ID (same as library would compute)
    return completeEvent
  }

  async sendMessage(
    recipientPublicKey: string,
    content: string,
    options: { kind?: number; tags?: string[][]; expiration?: ExpirationOptions | null } & ExpirationOptions = {}
  ): Promise<Rumor> {
    const { kind = CHAT_MESSAGE_KIND, tags = [] } = options

    // Build message exactly as library does (Session.ts sendEvent)
    const now = Date.now()
    const builtTags = this.buildMessageTags(recipientPublicKey, tags)

    const rumor: Rumor = {
      content,
      kind,
      created_at: Math.floor(now / 1000),
      tags: builtTags,
      pubkey: this.ourPublicKey,
      id: "", // Will compute next
    }

    if (!rumor.tags.some(([k]) => k === "ms")) {
      rumor.tags.push(["ms", String(now)])
    }

    // Expiration defaults can be configured per peer/group, but some inner rumor kinds must never expire.
    if (kind !== GROUP_METADATA_KIND && kind !== CHAT_SETTINGS_KIND) {
      const nowSeconds = Math.floor(now / 1000)

      const groupId = builtTags.find(t => t[0] === "l")?.[1]

      // Determine per-send expiration override:
      // - `expiration: null` disables expiration entirely (even if defaults exist)
      // - `expiration: {…}` overrides defaults
      // - legacy `expiresAt` / `ttlSeconds` on the options object are treated as an override when provided
      let expirationOverride: ExpirationOptions | null | undefined = options.expiration
      const legacyOverride =
        options.expiresAt !== undefined || options.ttlSeconds !== undefined

      if (expirationOverride === undefined && legacyOverride) {
        expirationOverride = { expiresAt: options.expiresAt, ttlSeconds: options.ttlSeconds }
      }

      if (expirationOverride !== null) {
        let disabledByPolicy = false
        let effective: ExpirationOptions | undefined

        if (expirationOverride !== undefined) {
          effective = expirationOverride
        } else if (groupId && this.groupExpiration.has(groupId)) {
          const v = this.groupExpiration.get(groupId)
          if (v === null) disabledByPolicy = true
          else effective = v
        } else if (this.peerExpiration.has(recipientPublicKey)) {
          const v = this.peerExpiration.get(recipientPublicKey)
          if (v === null) disabledByPolicy = true
          else effective = v
        } else {
          effective = this.defaultExpiration
        }

        if (!disabledByPolicy && effective) {
          const expiresAt = resolveExpirationSeconds(effective, nowSeconds)
          if (expiresAt !== undefined) {
            upsertExpirationTag(rumor.tags, expiresAt)
          }
        }
      }
    }

    rumor.id = getEventHash(rumor)

    // Use sendEvent for actual sending (includes queueing)
    // Note: sendEvent is not awaited to maintain backward compatibility
    // The message is queued and will be sent when sessions are established
    this.sendEvent(recipientPublicKey, rumor).catch(() => {})

    return rumor
  }

  /**
   * Send an encrypted 1:1 chat settings event (inner kind 10448).
   *
   * Settings events themselves should never expire; they are sent without a NIP-40 expiration tag.
   */
  async sendChatSettings(
    recipientPublicKey: string,
    messageTtlSeconds: ChatSettingsPayloadV1["messageTtlSeconds"]
  ): Promise<Rumor> {
    const payload: ChatSettingsPayloadV1 = {
      type: "chat-settings",
      v: 1,
      messageTtlSeconds,
    }
    return this.sendMessage(recipientPublicKey, JSON.stringify(payload), {
      kind: CHAT_SETTINGS_KIND,
      expiration: null,
    })
  }

  /**
   * Convenience: set per-peer disappearing-message TTL and notify the peer via a settings event.
   *
   * `messageTtlSeconds`:
   * - `> 0`: set per-peer ttlSeconds
   * - `0` or `null`: disable per-peer expiration even if a global default exists
   * - `undefined`: clear per-peer override (fall back to global default)
   */
  async setChatSettingsForPeer(
    peerPubkey: string,
    messageTtlSeconds: ChatSettingsPayloadV1["messageTtlSeconds"]
  ): Promise<Rumor> {
    if (messageTtlSeconds === undefined) {
      await this.setExpirationForPeer(peerPubkey, undefined)
    } else if (messageTtlSeconds === null || messageTtlSeconds === 0) {
      await this.setExpirationForPeer(peerPubkey, null)
    } else {
      await this.setExpirationForPeer(peerPubkey, { ttlSeconds: messageTtlSeconds })
    }

    return this.sendChatSettings(peerPubkey, messageTtlSeconds)
  }

  async sendReceipt(
    recipientPublicKey: string,
    receiptType: ReceiptType,
    messageIds: string[]
  ): Promise<Rumor | undefined> {
    if (messageIds.length === 0) return
    return this.sendMessage(recipientPublicKey, receiptType, {
      kind: RECEIPT_KIND,
      tags: messageIds.map((id) => ["e", id]),
    })
  }

  async sendTyping(recipientPublicKey: string): Promise<Rumor> {
    return this.sendMessage(recipientPublicKey, "typing", {
      kind: TYPING_KIND,
    })
  }

  private maybeAutoAdoptChatSettings(event: Rumor, fromOwnerPubkey: string): void {
    if (!this.autoAdoptChatSettings) return
    if (event.kind !== CHAT_SETTINGS_KIND) return

    let payload: unknown
    try {
      payload = JSON.parse(event.content)
    } catch {
      return
    }

    const p = payload as Partial<ChatSettingsPayloadV1>
    if (p?.type !== "chat-settings" || p?.v !== 1) return

    const recipientP = event.tags?.find((t) => t[0] === "p")?.[1]

    // Determine which peer this setting applies to:
    // - for incoming messages, `fromOwnerPubkey` is the peer
    // - for sender-copy sync across our own devices, `["p", <peer>]` indicates the peer
    const us = this.ownerPublicKey
    const peer =
      recipientP && recipientP !== us
        ? recipientP
        : fromOwnerPubkey && fromOwnerPubkey !== us
          ? fromOwnerPubkey
          : undefined
    if (!peer || peer === us) return

    const ttl = (p as ChatSettingsPayloadV1).messageTtlSeconds

    // Adopt:
    // - number > 0: set per-peer ttlSeconds
    // - 0 or null: disable per-peer expiration (even if a global default exists)
    // - undefined: clear per-peer override (fall back to global default)
    if (ttl === undefined) {
      this.setExpirationForPeer(peer, undefined).catch(() => {})
      return
    }

    if (ttl === null) {
      this.setExpirationForPeer(peer, null).catch(() => {})
      return
    }

    if (
      typeof ttl !== "number" ||
      !Number.isFinite(ttl) ||
      !Number.isSafeInteger(ttl) ||
      ttl < 0
    ) {
      return
    }

    if (ttl === 0) {
      this.setExpirationForPeer(peer, null).catch(() => {})
      return
    }

    this.setExpirationForPeer(peer, { ttlSeconds: ttl }).catch(() => {})
  }

  private buildMessageTags(
    recipientPublicKey: string,
    extraTags: string[][]
  ): string[][] {
    const hasRecipientPTag = extraTags.some(
      (tag) => tag[0] === "p" && tag[1] === recipientPublicKey
    )
    const tags = hasRecipientPTag
      ? [...extraTags]
      : [["p", recipientPublicKey], ...extraTags]
    return tags
  }

  private storeUserRecord(publicKey: string) {
    const userRecord = this.userRecords.get(publicKey)
    const devices = Array.from(userRecord?.devices.entries() || [])
    const serializeSession = (session: Session): StoredSessionEntry => ({
      name: session.name,
      state: serializeSessionState(session.state)
    })

    const data: StoredUserRecord = {
      publicKey: publicKey,
      devices: devices.map(
        ([, device]) => ({
          deviceId: device.deviceId,
          activeSession: device.activeSession
            ? serializeSession(device.activeSession)
            : null,
          inactiveSessions: device.inactiveSessions.map(serializeSession),
          createdAt: device.createdAt,
        })
      ),
      appKeys: userRecord?.appKeys?.serialize(),
    }
    const key = this.userRecordKey(publicKey)
    const prev = this.userRecordWriteChain.get(key) || Promise.resolve()
    const next = prev
      .catch(() => {})
      .then(() => this.storage.put(key, data))
    this.userRecordWriteChain.set(key, next)
    return next
  }

  private loadUserRecord(publicKey: string) {
    return this.storage
      .get<StoredUserRecord>(this.userRecordKey(publicKey))
      .then((data) => {
        if (!data) return
        const userRecord = this.getOrCreateUserRecord(publicKey)
        userRecord.close()
        userRecord.devices.clear()

        const deserializeSession = (entry: StoredSessionEntry): Session => {
          const session = new Session(this.nostrSubscribe, deserializeSessionState(entry.state))
          session.name = entry.name
          this.processedInviteResponses.add(entry.name)
          return session
        }

        let appKeys: AppKeys | undefined
        if (data.appKeys) {
          try {
            appKeys = AppKeys.deserialize(data.appKeys)
          } catch {
            // Failed to deserialize AppKeys
          }
        }

        userRecord.setAppKeys(appKeys)

        // Rebuild delegateToOwner mapping from persisted AppKeys
        if (appKeys) {
          for (const device of appKeys.getAllDevices()) {
            if (device.identityPubkey) {
              this.delegateToOwner.set(device.identityPubkey, publicKey)
            }
          }
        }

        for (const deviceData of data.devices) {
          const {
            deviceId,
            activeSession: serializedActive,
            inactiveSessions: serializedInactive,
            createdAt,
          } = deviceData

          try {
            const device = userRecord.ensureDevice(deviceId, createdAt)

            for (const session of serializedInactive.map(deserializeSession).reverse()) {
              device.installSession(session, true, { persist: false })
            }

            if (serializedActive) {
              device.installSession(deserializeSession(serializedActive), false, { persist: false })
            }
          } catch {
            // Failed to deserialize session
          }
        }
      })
      .catch(() => {
        // Failed to load user record
      })
  }

  private loadAllUserRecords() {
    const prefix = this.userRecordKeyPrefix()
    return this.storage.list(prefix).then((keys) => {
      return Promise.all(
        keys.map((key) => {
          const publicKey = key.slice(prefix.length)
          return this.loadUserRecord(publicKey)
        })
      )
    })
  }

  private async runMigrations() {
    // Run migrations sequentially
    let version = await this.storage.get<string>(this.versionKey())

    // First migration
    if (!version) {
      // Delete old invite data (legacy format no longer supported)
      const oldInvitePrefix = "invite/"
      const inviteKeys = await this.storage.list(oldInvitePrefix)
      await Promise.all(inviteKeys.map((key) => this.storage.del(key)))

      // Migrate old user records (clear sessions, keep device records)
      const oldUserRecordPrefix = "user/"
      const sessionKeys = await this.storage.list(oldUserRecordPrefix)
      await Promise.all(
        sessionKeys.map(async (key) => {
          try {
            const publicKey = key.slice(oldUserRecordPrefix.length)
            const userRecordData = await this.storage.get<StoredUserRecord>(key)
            if (userRecordData) {
              const newKey = this.userRecordKey(publicKey)
              const newUserRecordData: StoredUserRecord = {
                publicKey: userRecordData.publicKey,
                devices: userRecordData.devices.map((device) => ({
                  deviceId: device.deviceId,
                  activeSession: null,
                  createdAt: device.createdAt,
                  inactiveSessions: [],
                })),
              }
              await this.storage.put(newKey, newUserRecordData)
              await this.storage.del(key)
            }
          } catch {
            // Migration error for user record
          }
        })
      )

      version = "1"
      await this.storage.put(this.versionKey(), version)
    }
  }
}
