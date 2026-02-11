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
import { AppKeys, DeviceEntry } from "./AppKeys"
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

export type OnEventCallback = (event: Rumor, from: string) => void

/**
 * Credentials for the invite handshake - used to listen for and decrypt invite responses
 */
export interface InviteCredentials {
  ephemeralKeypair: { publicKey: string; privateKey: Uint8Array }
  sharedSecret: string
}

export interface DeviceRecord {
  deviceId: string
  activeSession?: Session
  inactiveSessions: Session[]
  createdAt: number
}

export interface UserRecord {
  publicKey: string
  devices: Map<string, DeviceRecord>
  /** Full AppKeys for this user - single source of truth for device list */
  appKeys?: AppKeys
}

interface StoredSessionEntry {
  name: string
  state: string
}

interface StoredDeviceRecord {
  deviceId: string
  activeSession: StoredSessionEntry | null
  inactiveSessions: StoredSessionEntry[]
  createdAt: number
}

interface StoredUserRecord {
  publicKey: string
  devices: StoredDeviceRecord[]
  appKeys?: string
}

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

  // Subscriptions
  private ourInviteResponseSubscription: Unsubscribe | null = null
  private inviteSubscriptions: Map<string, Unsubscribe> = new Map()
  private sessionSubscriptions: Map<string, Unsubscribe> = new Map()

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

          this.attachSessionSubscription(ownerPubkey, deviceRecord, session, true)
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
  private getOrCreateUserRecord(userPubkey: string): UserRecord {
    let rec = this.userRecords.get(userPubkey)
    if (!rec) {
      rec = { publicKey: userPubkey, devices: new Map() }
      this.userRecords.set(userPubkey, rec)
    }
    return rec
  }

  private upsertDeviceRecord(userRecord: UserRecord, deviceId: string): DeviceRecord {
    if (!deviceId) {
      throw new Error("Device record must include a deviceId")
    }
    const existing = userRecord.devices.get(deviceId)
    if (existing) {
      return existing
    }

    const deviceRecord: DeviceRecord = {
      deviceId,
      inactiveSessions: [],
      createdAt: Date.now(),
    }
    userRecord.devices.set(deviceId, deviceRecord)
    return deviceRecord
  }

  private sessionKey(userPubkey: string, deviceId: string, sessionName: string) {
    return `${this.sessionKeyPrefix(userPubkey)}${deviceId}/${sessionName}`
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

  private subscribeToUserAppKeys(
    pubkey: string,
    onAppKeys: (list: AppKeys) => void
  ): Unsubscribe {
    // Track the latest created_at to skip stale replaceable events that
    // relays may deliver out of order or alongside their replacements.
    let latestCreatedAt = 0
    return this.nostrSubscribe(
      {
        kinds: [APP_KEYS_EVENT_KIND],
        authors: [pubkey],
        "#d": ["double-ratchet/app-keys"],
      },
      (event) => {
        // AppKeys events are replaceable (kind 30078).  Skip events that are
        // strictly older than the most recent one we've already processed.
        if (event.created_at < latestCreatedAt) return
        latestCreatedAt = event.created_at

        try {
          const list = AppKeys.fromEvent(event)
          // Update delegate mapping whenever we receive an AppKeys
          this.updateDelegateMapping(pubkey, list)
          onAppKeys(list)
        } catch {
          // Invalid event, ignore
        }
      }
    )
  }

  private static MAX_INACTIVE_SESSIONS = 10

  private attachSessionSubscription(
    userPubkey: string,
    deviceRecord: DeviceRecord,
    session: Session,
    // Set to true if only handshake -> not yet sendable -> will be promoted on message
    inactive: boolean = false
  ): void {
    const key = this.sessionKey(userPubkey, deviceRecord.deviceId, session.name)
    if (this.sessionSubscriptions.has(key)) {
      return
    }

    const dr = deviceRecord

    // Promote a session to active when it receives a message
    // Current active goes to top of inactive queue
    const promoteToActive = (nextSession: Session) => {
      const current = dr.activeSession

      // Already active, nothing to do
      if (current === nextSession || current?.name === nextSession.name) {
        return
      }

      // Remove nextSession from inactive if present
      dr.inactiveSessions = dr.inactiveSessions.filter(
        (s) => s !== nextSession && s.name !== nextSession.name
      )

      // Move current active to top of inactive queue
      if (current) {
        dr.inactiveSessions.unshift(current)
      }

      // Set new active
      dr.activeSession = nextSession

      // Trim inactive queue to max size (remove oldest from end)
      if (dr.inactiveSessions.length > SessionManager.MAX_INACTIVE_SESSIONS) {
        const removed = dr.inactiveSessions.splice(SessionManager.MAX_INACTIVE_SESSIONS)
        // Unsubscribe from removed sessions
        for (const s of removed) {
          this.removeSessionSubscription(userPubkey, dr.deviceId, s.name)
        }
      }
    }

    // Add new session: if inactive, add to top of inactive queue; otherwise set as active
    if (inactive) {
      const alreadyTracked = dr.inactiveSessions.some(
        (s) => s === session || s.name === session.name
      )
      if (!alreadyTracked) {
        // Add to top of inactive queue
        dr.inactiveSessions.unshift(session)
        // Trim to max size
        if (dr.inactiveSessions.length > SessionManager.MAX_INACTIVE_SESSIONS) {
          const removed = dr.inactiveSessions.splice(SessionManager.MAX_INACTIVE_SESSIONS)
          for (const s of removed) {
            this.removeSessionSubscription(userPubkey, dr.deviceId, s.name)
          }
        }
      }
    } else {
      promoteToActive(session)
    }

    // Subscribe to session events - when message received, promote to active
    const unsub = session.onEvent((event) => {
      // Verify sender device is still authorized
      const senderOwner = this.resolveToOwner(deviceRecord.deviceId)
      if (senderOwner !== deviceRecord.deviceId && !this.isDeviceAuthorized(senderOwner, deviceRecord.deviceId)) {
        return
      }

      this.maybeAutoAdoptChatSettings(event, userPubkey)

      for (const cb of this.internalSubscriptions) cb(event, userPubkey)
      promoteToActive(session)
      this.storeUserRecord(userPubkey).catch(() => {})
    })
    this.storeUserRecord(userPubkey).catch(() => {})
    this.sessionSubscriptions.set(key, unsub)
  }

  private attachAppKeysSubscription(
    userPubkey: string,
    onAppKeys?: (appKeys: AppKeys) => void | Promise<void>
  ): void {
    const key = `appkeys:${userPubkey}`
    if (this.inviteSubscriptions.has(key)) return

    const unsubscribe = this.subscribeToUserAppKeys(
      userPubkey,
      async (appKeys) => {
        if (onAppKeys) await onAppKeys(appKeys)
      }
    )

    this.inviteSubscriptions.set(key, unsubscribe)
  }

  setupUser(userPubkey: string) {
    const userRecord = this.getOrCreateUserRecord(userPubkey)

    // Track which device identities we've subscribed to for invites
    const subscribedDeviceIdentities = new Set<string>()
    // Track devices currently being accepted (to prevent duplicate acceptance)
    const pendingAcceptances = new Set<string>()

    /**
     * Accept an invite from a device.
     * The invite is fetched separately from the device's own Invite event.
     */
    const acceptInviteFromDevice = async (
      device: DeviceEntry,
      invite: Invite
    ) => {
      // Double-check for active session (race condition guard)
      // Another concurrent call may have already established a session
      const existingRecord = userRecord.devices.get(device.identityPubkey)
      if (existingRecord?.activeSession) {
        return
      }

      // Add device record IMMEDIATELY to prevent duplicate acceptance from race conditions
      // Use identityPubkey as the device identifier
      const deviceRecord = this.upsertDeviceRecord(userRecord, device.identityPubkey)

      const encryptor = this.identityKey instanceof Uint8Array ? this.identityKey : this.identityKey.encrypt
      // ourPublicKey serves as both identity and device ID
      const { session, event } = await invite.accept(
        this.nostrSubscribe,
        this.ourPublicKey,
        encryptor,
        this.ownerPublicKey
      )
      return this.nostrPublish(event)
        .then(() => {
          this.attachSessionSubscription(userPubkey, deviceRecord, session)
        })
        .then(() => this.flushMessageQueue(device.identityPubkey))
        .catch(() => {})
    }

    /**
     * Subscribe to a device's Invite event and accept it when received.
     */
    const subscribeToDeviceInvite = (device: DeviceEntry) => {
      // identityPubkey is the device identifier
      const deviceKey = device.identityPubkey
      if (subscribedDeviceIdentities.has(deviceKey)) {
        return
      }
      subscribedDeviceIdentities.add(deviceKey)

      // Already have a record with active session for this device? Skip.
      const existingRecord = userRecord.devices.get(device.identityPubkey)
      if (existingRecord?.activeSession) {
        return
      }

      const inviteSubKey = `invite:${device.identityPubkey}`
      if (this.inviteSubscriptions.has(inviteSubKey)) {
        return
      }

      // Subscribe to this device's Invite event
      const unsub = Invite.fromUser(device.identityPubkey, this.nostrSubscribe, async (invite) => {
        // Verify the invite is for this device (identityPubkey is the device identifier)
        if (invite.deviceId !== device.identityPubkey) {
          return
        }

        // Skip if we already have an active session (race condition guard)
        const existingDeviceRecord = userRecord.devices.get(device.identityPubkey)
        if (existingDeviceRecord?.activeSession) {
          return
        }

        // Skip if acceptance is already in progress (race condition guard)
        if (pendingAcceptances.has(device.identityPubkey)) {
          return
        }

        pendingAcceptances.add(device.identityPubkey)
        try {
          await acceptInviteFromDevice(device, invite)
        } finally {
          pendingAcceptances.delete(device.identityPubkey)
        }
      })

      this.inviteSubscriptions.set(inviteSubKey, unsub)
    }

    this.attachAppKeysSubscription(userPubkey, async (appKeys) => {
      const devices = appKeys.getAllDevices()
      const activeDeviceIds = new Set(devices.map(d => d.identityPubkey))

      // Handle devices no longer in list (revoked or AppKeys recreated from scratch)
      const userRecord = this.userRecords.get(userPubkey)
      if (userRecord) {
        for (const [deviceId] of userRecord.devices) {
          if (!activeDeviceIds.has(deviceId)) {
            // Remove from tracking so device can be re-subscribed if re-added
            subscribedDeviceIdentities.delete(deviceId)
            const inviteSubKey = `invite:${deviceId}`
            const inviteUnsub = this.inviteSubscriptions.get(inviteSubKey)
            if (inviteUnsub) {
              inviteUnsub()
              this.inviteSubscriptions.delete(inviteSubKey)
            }
            await this.cleanupDevice(userPubkey, deviceId)
          }
        }
      }

      // Expand DiscoveryQueue → MessageQueue BEFORE subscribing to invites
      // (invite acceptance triggers flushMessageQueue, so entries must be ready).
      // After expansion, remove the discovery entry to prevent unbounded growth.
      // Any duplicates that may be introduced are deduplicated by event ID during reads,
      // and we remove all duplicates for a given eventId on successful publish.
      const discoveryEntries = await this.discoveryQueue.getForTarget(userPubkey)
      if (discoveryEntries.length > 0) {
        for (const entry of discoveryEntries) {
          try {
            for (const device of devices) {
              if (device.identityPubkey && device.identityPubkey !== this.deviceId) {
                await this.messageQueue.add(device.identityPubkey, entry.event)
              }
            }
          } finally {
            // Remove the discovery entry after expansion regardless of per-device add outcomes
            await this.discoveryQueue.remove(entry.id).catch(() => {})
          }
        }
      }

      // For each device in AppKeys, subscribe to their Invite event
      for (const device of devices) {
        subscribeToDeviceInvite(device)
      }

      // Flush MessageQueue for devices that already have active sessions
      for (const device of devices) {
        if (device.identityPubkey && device.identityPubkey !== this.deviceId) {
          const deviceRecord = this.userRecords.get(userPubkey)?.devices.get(device.identityPubkey)
          if (deviceRecord?.activeSession) {
            this.flushMessageQueue(device.identityPubkey).catch(() => {})
          }
        }
      }
    })
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
    return this.userRecords
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
    for (const unsubscribe of this.inviteSubscriptions.values()) {
      unsubscribe()
    }

    for (const unsubscribe of this.sessionSubscriptions.values()) {
      unsubscribe()
    }

    this.ourInviteResponseSubscription?.()
  }

  deactivateCurrentSessions(publicKey: string) {
    const userRecord = this.userRecords.get(publicKey)
    if (!userRecord) return
    for (const device of userRecord.devices.values()) {
      if (device.activeSession) {
        device.inactiveSessions.push(device.activeSession)
        device.activeSession = undefined
      }
    }
    this.storeUserRecord(publicKey).catch(() => {})
  }

  async deleteUser(userPubkey: string): Promise<void> {
    await this.init()

    const userRecord = this.userRecords.get(userPubkey)

    if (userRecord) {
      for (const device of userRecord.devices.values()) {
        if (device.activeSession) {
          this.removeSessionSubscription(
            userPubkey,
            device.deviceId,
            device.activeSession.name
          )
        }

        for (const session of device.inactiveSessions) {
          this.removeSessionSubscription(userPubkey, device.deviceId, session.name)
        }
      }

      this.userRecords.delete(userPubkey)
    }

    const appKeysKey = `appkeys:${userPubkey}`
    const appKeysUnsub = this.inviteSubscriptions.get(appKeysKey)
    if (appKeysUnsub) {
      appKeysUnsub()
      this.inviteSubscriptions.delete(appKeysKey)
    }

    // Remove discovery queue entries for this owner
    await this.discoveryQueue.removeForTarget(userPubkey)
    // Remove message queue entries for all known devices
    if (userRecord) {
      for (const [deviceId] of userRecord.devices) {
        await this.messageQueue.removeForTarget(deviceId)
      }
    }

    await Promise.allSettled([
      this.deleteUserSessionsFromStorage(userPubkey),
      this.storage.del(this.userRecordKey(userPubkey)),
    ])
  }

  private removeSessionSubscription(
    userPubkey: string,
    deviceId: string,
    sessionName: string
  ) {
    const key = this.sessionKey(userPubkey, deviceId, sessionName)
    const unsubscribe = this.sessionSubscriptions.get(key)
    if (unsubscribe) {
      unsubscribe()
      this.sessionSubscriptions.delete(key)
    }
  }

  private async deleteUserSessionsFromStorage(userPubkey: string): Promise<void> {
    const prefix = this.sessionKeyPrefix(userPubkey)
    const keys = await this.storage.list(prefix)
    await Promise.all(keys.map((key) => this.storage.del(key)))
  }

  private async flushMessageQueue(deviceIdentity: string): Promise<void> {
    const entries = await this.messageQueue.getForTarget(deviceIdentity)
    if (entries.length === 0) {
      return
    }

    const ownerPubkey = this.resolveToOwner(deviceIdentity)
    const userRecord = this.userRecords.get(ownerPubkey)
    const device = userRecord?.devices.get(deviceIdentity)
    if (!device?.activeSession) {
      return
    }

    for (const entry of entries) {
      try {
        const { event: verifiedEvent } = device.activeSession.sendEvent(entry.event)
        await this.nostrPublish(verifiedEvent)
        // Remove all queue entries for this device/event.id pair (handle potential duplicates)
        await this.messageQueue.removeByTargetAndEventId(deviceIdentity, entry.event.id)
      } catch (e) {
        // Keep entry for future retry
      }
    }
    await this.storeUserRecord(ownerPubkey).catch(() => {})
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

      if (devices.length > 0) {
        // AppKeys known: queue per-device, skip discovery to avoid growth
        for (const device of devices) {
          if (device.identityPubkey && device.identityPubkey !== this.deviceId) {
            await this.messageQueue.add(device.identityPubkey, completeEvent)
          }
        }
      } else {
        // Unknown device list: queue a discovery entry (expanded later when AppKeys arrive)
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

  private async cleanupDevice(publicKey: string, deviceId: string): Promise<void> {
    const userRecord = this.userRecords.get(publicKey)
    if (!userRecord) return
    const deviceRecord = userRecord.devices.get(deviceId)
    if (!deviceRecord) return

    // Unsubscribe from sessions
    if (deviceRecord.activeSession) {
      this.removeSessionSubscription(publicKey, deviceId, deviceRecord.activeSession.name)
    }
    for (const session of deviceRecord.inactiveSessions) {
      this.removeSessionSubscription(publicKey, deviceId, session.name)
    }

    // Remove delegate mapping
    this.delegateToOwner.delete(deviceId)

    // Delete the device record entirely
    userRecord.devices.delete(deviceId)
    await this.storeUserRecord(publicKey).catch(() => {})
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

        const devices = new Map<string, DeviceRecord>()

        const deserializeSession = (entry: StoredSessionEntry): Session => {
          const session = new Session(this.nostrSubscribe, deserializeSessionState(entry.state))
          session.name = entry.name
          this.processedInviteResponses.add(entry.name)
          return session
        }

        for (const deviceData of data.devices) {
          const {
            deviceId,
            activeSession: serializedActive,
            inactiveSessions: serializedInactive,
            createdAt,
          } = deviceData

          try {
            const activeSession = serializedActive
              ? deserializeSession(serializedActive)
              : undefined

            const inactiveSessions = serializedInactive.map(deserializeSession)

            devices.set(deviceId, {
              deviceId,
              activeSession,
              inactiveSessions,
              createdAt,
            })
          } catch {
            // Failed to deserialize session
          }
        }

        let appKeys: AppKeys | undefined
        if (data.appKeys) {
          try {
            appKeys = AppKeys.deserialize(data.appKeys)
          } catch {
            // Failed to deserialize AppKeys
          }
        }

        this.userRecords.set(publicKey, {
          publicKey: data.publicKey,
          devices,
          appKeys,
        })

        // Rebuild delegateToOwner mapping from persisted AppKeys
        if (appKeys) {
          for (const device of appKeys.getAllDevices()) {
            if (device.identityPubkey) {
              this.delegateToOwner.set(device.identityPubkey, publicKey)
            }
          }
        }

        for (const device of devices.values()) {
          const { deviceId, activeSession, inactiveSessions } = device
          if (!deviceId) continue

          for (const session of inactiveSessions.reverse()) {
            this.attachSessionSubscription(publicKey, device, session, true)  // Restore as inactive
          }
          if (activeSession) {
            this.attachSessionSubscription(publicKey, device, activeSession)  // Restore as active
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
