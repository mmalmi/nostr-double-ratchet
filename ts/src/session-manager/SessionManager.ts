import { getEventHash } from "nostr-tools"

import { AppKeys } from "../AppKeys"
import { GROUP_METADATA_KIND } from "../GroupMeta"
import { Invite } from "../Invite"
import { MessageQueue } from "../MessageQueue"
import { Session } from "../Session"
import { InMemoryStorageAdapter, StorageAdapter } from "../StorageAdapter"
import {
  APP_KEYS_EVENT_KIND,
  CHAT_MESSAGE_KIND,
  CHAT_SETTINGS_KIND,
  ExpirationOptions,
  IdentityKey,
  NostrPublish,
  NostrSubscribe,
  RECEIPT_KIND,
  ReceiptType,
  Rumor,
  TYPING_KIND,
  Unsubscribe,
  ChatSettingsPayloadV1,
} from "../types"
import { createSessionFromAccept, decryptInviteResponse } from "../inviteUtils"
import {
  deserializeSessionState,
  resolveExpirationSeconds,
  serializeSessionState,
  upsertExpirationTag,
} from "../utils"
import { UserRecord } from "./UserRecord"
import type {
  AcceptInviteOptions,
  AcceptInviteResult,
  InviteCredentials,
  NostrFacade,
  OnEventCallback,
  StoredSessionEntry,
  StoredUserRecord,
  UserSetupStatus,
} from "./types"

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
  private nostr: NostrFacade

  // Credentials for invite handshake
  private inviteKeys: InviteCredentials

  // Data
  private userRecords: Map<string, UserRecord> = new Map()
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
  // User records marked for deletion; dirty callbacks must not persist these keys.
  private deletedUserRecords: Set<string> = new Set()

  // Subscriptions
  private ourInviteResponseSubscription: Unsubscribe | null = null
  // Callbacks
  private internalSubscriptions: Set<OnEventCallback> = new Set()
  private userSetupSubscriptions: Map<string, Set<(status: UserSetupStatus) => void>> = new Map()

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
    this.nostr = {
      subscribe: this.nostrSubscribe,
      publish: async (event: Parameters<NostrPublish>[0]) => {
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
    ourUserRecord.ensureDevice(this.deviceId)

    // Start invite response listener BEFORE setting up users
    // This ensures we're listening when other devices respond to our invites
    this.startInviteResponseListener()

    // Setup sessions with our own devices and resume discovery for all known users
    Array.from(this.userRecords.keys()).forEach((pubkey) => {
      this.ensureUserSetup(pubkey)
    })
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
          const deviceRecord = userRecord.ensureDevice(decrypted.inviteeIdentity)

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
  private currentUserSetupStatus(ownerPubkey: string): UserSetupStatus {
    const userRecord = this.userRecords.get(ownerPubkey)
    const state = userRecord?.state || "new"
    const appKeysKnown = Boolean(userRecord?.appKeys)
    return {
      ownerPublicKey: ownerPubkey,
      state,
      ready: state === "ready",
      appKeysKnown,
    }
  }

  private emitUserSetupStatus(ownerPubkey: string): void {
    const callbacks = this.userSetupSubscriptions.get(ownerPubkey)
    if (!callbacks || callbacks.size === 0) {
      return
    }
    const status = this.currentUserSetupStatus(ownerPubkey)
    for (const callback of callbacks) {
      try {
        callback(status)
      } catch {
        // Ignore callback failures from app code.
      }
    }
  }

  private getOrCreateUserRecord(userPubkey: string): UserRecord {
    let rec = this.userRecords.get(userPubkey)
    if (!rec) {
      this.deletedUserRecords.delete(userPubkey)
      rec = new UserRecord(userPubkey, {
        manager: this,
        nostr: this.nostr,
        messageQueue: this.messageQueue,
        discoveryQueue: this.discoveryQueue,
        ourDeviceId: this.deviceId,
        ourOwnerPubkey: this.ownerPublicKey,
        identityKey: this.identityKey,
        onSetupStateChange: (ownerPubkey) => {
          this.emitUserSetupStatus(ownerPubkey)
        },
      })
      this.userRecords.set(userPubkey, rec)
      this.emitUserSetupStatus(userPubkey)
    }
    return rec
  }

  handleDeviceRumor(ownerPubkey: string, deviceId: string, rumor: Rumor): void {
    this.maybeAutoAdoptChatSettings(rumor, ownerPubkey)
    for (const cb of this.internalSubscriptions) {
      cb(rumor, ownerPubkey, { fromDeviceId: deviceId })
    }
  }

  persistUserRecord(ownerPubkey: string): void {
    this.storeUserRecord(ownerPubkey).catch(() => {})
  }

  removeDelegateMapping(deviceId: string): void {
    this.delegateToOwner.delete(deviceId)
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
  updateDelegateMapping(ownerPubkey: string, appKeys: AppKeys): void {
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

  private ensureUserSetup(userPubkey: string): void {
    const userRecord = this.getOrCreateUserRecord(userPubkey)
    userRecord.ensureSetup().catch(() => {})
  }

  onEvent(callback: OnEventCallback) {
    this.internalSubscriptions.add(callback)

    return () => {
      this.internalSubscriptions.delete(callback)
    }
  }

  /**
   * Subscribe to setup readiness updates for a chat peer/owner.
   * Callback is invoked immediately with current status.
   */
  onUserSetupStatus(
    userPubkey: string,
    callback: (status: UserSetupStatus) => void
  ): Unsubscribe {
    const ownerPubkey = this.resolveToOwner(userPubkey)
    let callbacks = this.userSetupSubscriptions.get(ownerPubkey)
    if (!callbacks) {
      callbacks = new Set()
      this.userSetupSubscriptions.set(ownerPubkey, callbacks)
    }
    callbacks.add(callback)

    callback(this.currentUserSetupStatus(ownerPubkey))

    return () => {
      const current = this.userSetupSubscriptions.get(ownerPubkey)
      if (!current) return
      current.delete(callback)
      if (current.size === 0) {
        this.userSetupSubscriptions.delete(ownerPubkey)
      }
    }
  }

  /**
   * Returns setup status for a chat peer/owner.
   */
  getUserSetupStatus(userPubkey: string): UserSetupStatus {
    const ownerPubkey = this.resolveToOwner(userPubkey)
    return this.currentUserSetupStatus(ownerPubkey)
  }

  /**
   * Returns true when the peer has known AppKeys and setup has finished.
   */
  isUserReady(userPubkey: string): boolean {
    return this.getUserSetupStatus(userPubkey).ready
  }

  /**
   * Start setup for a peer/owner now (fetch AppKeys, fan out queues, prepare sessions).
   * Returns current status after this setup attempt.
   */
  async startUserSetup(userPubkey: string): Promise<UserSetupStatus> {
    await this.init()
    const ownerPubkey = this.resolveToOwner(userPubkey)
    const userRecord = this.getOrCreateUserRecord(ownerPubkey)
    this.emitUserSetupStatus(ownerPubkey)
    await userRecord.ensureSetup().catch(() => {})
    const status = this.currentUserSetupStatus(ownerPubkey)
    this.emitUserSetupStatus(ownerPubkey)
    return status
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
    this.ourInviteResponseSubscription = null
  }

  async deleteChat(userPubkey: string): Promise<void> {
    return this.deleteUser(this.resolveToOwner(userPubkey))
  }

  private async deleteUser(userPubkey: string): Promise<void> {
    await this.init()

    const ownerPubkey = this.resolveToOwner(userPubkey)
    if (ownerPubkey === this.ownerPublicKey) return

    this.deletedUserRecords.add(ownerPubkey)
    const userRecord = this.userRecords.get(ownerPubkey)

    if (userRecord) {
      userRecord.close()
      for (const device of userRecord.devices.values()) {
        await device.revoke()
      }

      this.userRecords.delete(ownerPubkey)
      this.emitUserSetupStatus(ownerPubkey)
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
      this.deleteStoredUserRecord(ownerPubkey),
    ])
    this.emitUserSetupStatus(ownerPubkey)
  }

  private async deleteUserSessionsFromStorage(userPubkey: string): Promise<void> {
    const prefix = this.sessionKeyPrefix(userPubkey)
    const keys = await this.storage.list(prefix)
    await Promise.all(keys.map((key) => this.storage.del(key)))
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

    const ownerPublicKey =
      options.ownerPublicKey ||
      invite.ownerPubkey ||
      this.resolveToOwner(deviceId) ||
      deviceId

    const userRecord = this.getOrCreateUserRecord(ownerPublicKey)
    const deviceRecord = userRecord.ensureDevice(deviceId)
    if (deviceRecord.activeSession) {
      return { ownerPublicKey, deviceId, session: deviceRecord.activeSession }
    }

    // When an invite claims delegate ownership, verify against AppKeys when available.
    if (ownerPublicKey !== deviceId) {
      const appKeys = await this.fetchAppKeys(ownerPublicKey).catch(() => null)
      if (appKeys) {
        const isAuthorized = appKeys
          .getAllDevices()
          .some((device) => device.identityPubkey === deviceId)
        if (!isAuthorized) {
          throw new Error("Invite device is not authorized by owner AppKeys")
        }
        this.updateDelegateMapping(ownerPublicKey, appKeys)
      } else {
        const persistedAppKeys = this.userRecords.get(ownerPublicKey)?.appKeys
        const isAuthorized =
          persistedAppKeys
            ?.getAllDevices()
            .some((device) => device.identityPubkey === deviceId) ?? false
        if (persistedAppKeys && !isAuthorized) {
          throw new Error("Invite device is not authorized by persisted AppKeys")
        }
      }
    }

    const session = await deviceRecord.acceptInvite(invite)
    this.delegateToOwner.set(deviceId, ownerPublicKey)
    userRecord.ensureSetup().catch(() => {})
    await this.storeUserRecord(ownerPublicKey).catch(() => {})

    return { ownerPublicKey, deviceId, session }
  }

  private async sendRumor(
    recipientIdentityKey: string,
    event: Partial<Rumor>
  ): Promise<Rumor | undefined> {
    await this.init()

    const completeEvent = event as Rumor
    const recipientOwnerPubkey = this.resolveToOwner(recipientIdentityKey)
    const targets = new Set<string>([recipientOwnerPubkey, this.ownerPublicKey])

    // Queue first, then nudge each user to progress itself.
    for (const ownerTarget of targets) {
      const userRecord = this.getOrCreateUserRecord(ownerTarget)
      await userRecord.queueOutboundMessage(completeEvent)
      userRecord.ensureSetup().catch(() => {})
    }

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

    // Use sendRumor for actual sending (includes queueing)
    // Note: sendRumor is not awaited to maintain backward compatibility
    // The message is queued and will be sent when sessions are established
    this.sendRumor(recipientPublicKey, rumor).catch(() => {})

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
    const key = this.userRecordKey(publicKey)
    const prev = this.userRecordWriteChain.get(key) || Promise.resolve()
    const next = prev
      .catch(() => {})
      .then(async () => {
        if (this.deletedUserRecords.has(publicKey)) {
          return
        }

        const userRecord = this.userRecords.get(publicKey)
        if (!userRecord) {
          return
        }

        const serializeSession = (session: Session): StoredSessionEntry => ({
          name: session.name,
          state: serializeSessionState(session.state)
        })

        const devices = Array.from(userRecord.devices.values())
        const data: StoredUserRecord = {
          publicKey: publicKey,
          devices: devices.map((device) => ({
            deviceId: device.deviceId,
            activeSession: device.activeSession
              ? serializeSession(device.activeSession)
              : null,
            inactiveSessions: device.inactiveSessions.map(serializeSession),
            createdAt: device.createdAt,
          })),
          appKeys: userRecord.appKeys?.serialize(),
        }

        await this.storage.put(key, data)
      })
    this.userRecordWriteChain.set(key, next)
    return next
  }

  private deleteStoredUserRecord(publicKey: string) {
    const key = this.userRecordKey(publicKey)
    const prev = this.userRecordWriteChain.get(key) || Promise.resolve()
    const next = prev
      .catch(() => {})
      .then(() => this.storage.del(key))
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

            const inactiveSessions = serializedInactive.map(deserializeSession)
            for (const session of inactiveSessions.reverse()) {
              device.installSession(session, true, { persist: false })
            }

            if (serializedActive) {
              const activeSession = deserializeSession(serializedActive)
              device.installSession(activeSession, false, { persist: false })
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
