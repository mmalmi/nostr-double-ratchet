import {
  IdentityKey,
  NostrSubscribe,
  NostrPublish,
  Rumor,
  Unsubscribe,
  APPLICATION_KEYS_EVENT_KIND,
  CHAT_MESSAGE_KIND,
} from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { ApplicationKeys, DeviceEntry } from "./ApplicationKeys"
import { Invite } from "./Invite"
import { Session } from "./Session"
import { serializeSessionState, deserializeSessionState } from "./utils"
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
  /** Device identity pubkeys from ApplicationKeys - used to rebuild delegateToOwner on load */
  knownDeviceIdentities: string[]
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
  knownDeviceIdentities?: string[]
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
  private messageHistory: Map<string, Rumor[]> = new Map()
  // Map delegate device pubkeys to their owner's pubkey
  private delegateToOwner: Map<string, string> = new Map()
  // Track processed InviteResponse event IDs to prevent replay
  private processedInviteResponses: Set<string> = new Set()

  // Subscriptions
  private ourInviteResponseSubscription: Unsubscribe | null = null
  private inviteSubscriptions: Map<string, Unsubscribe> = new Map()
  private sessionSubscriptions: Map<string, Unsubscribe> = new Map()

  // Callbacks
  private internalSubscriptions: Set<OnEventCallback> = new Set()

  // Initialization flag
  private initialized: boolean = false

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

    // Add our own device to user record to prevent accepting our own invite
    // Use ownerPublicKey so delegates are added to the owner's record
    const ourUserRecord = this.getOrCreateUserRecord(this.ownerPublicKey)
    this.upsertDeviceRecord(ourUserRecord, this.deviceId)

    // Start invite response listener BEFORE setting up users
    // This ensures we're listening when other devices respond to our invites
    this.startInviteResponseListener()
    // Setup sessions with our own other devices
    // Use ownerPublicKey to find sibling devices (important for delegates)
    this.setupUser(this.ownerPublicKey)
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

          // Verify the device is authorized by fetching owner's ApplicationKeys
          const applicationKeys = await this.fetchApplicationKeys(claimedOwner)

          if (!applicationKeys) {
            // No ApplicationKeys found - check cached device identities as fallback
            const cachedRecord = this.userRecords.get(claimedOwner)
            const cachedIdentities = cachedRecord?.knownDeviceIdentities || []

            if (cachedIdentities.includes(decrypted.inviteeIdentity)) {
              // Device is in cached list - allow (this handles restart scenarios)
            } else if (decrypted.inviteeIdentity === claimedOwner) {
              // Single-device user (device = owner), proceed without ApplicationKeys
            } else {
              return
            }
          } else {
            // Check that the responding device is actually in the owner's ApplicationKeys
            const deviceInList = applicationKeys.getAllDevices().some(
              d => d.identityPubkey === decrypted.inviteeIdentity
            )
            if (!deviceInList) {
              return
            }

            // Update delegate mapping with verified ApplicationKeys
            this.updateDelegateMapping(claimedOwner, applicationKeys)
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
          // Failed to decrypt invite response
        }
      }
    )
  }

  /**
   * Fetch a user's ApplicationKeys from relays.
   * Returns null if not found within timeout.
   */
  private fetchApplicationKeys(pubkey: string, timeoutMs = 2000): Promise<ApplicationKeys | null> {
    return new Promise((resolve) => {
      let latestEvent: { created_at: number; applicationKeys: ApplicationKeys } | null = null
      let resolved = false

      // Use a short initial delay before resolving to allow event delivery
      const resolveResult = () => {
        if (resolved) return
        resolved = true
        unsubscribe()
        resolve(latestEvent?.applicationKeys ?? null)
      }

      // Start timeout
      const timeout = setTimeout(resolveResult, timeoutMs)

      const unsubscribe = this.nostrSubscribe(
        {
          kinds: [APPLICATION_KEYS_EVENT_KIND],
          authors: [pubkey],
          "#d": ["double-ratchet/application-keys"],
        },
        (event) => {
          if (resolved) return
          try {
            const applicationKeys = ApplicationKeys.fromEvent(event)
            // Use >= to prefer later-delivered events when timestamps are equal
            // This handles replaceable events created within the same second
            if (!latestEvent || event.created_at >= latestEvent.created_at) {
              latestEvent = { created_at: event.created_at, applicationKeys }
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
      rec = { publicKey: userPubkey, devices: new Map(), knownDeviceIdentities: [] }
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
   * Update the delegate-to-owner mapping from an ApplicationKeys.
   * Extracts delegate device pubkeys and maps them to the owner.
   * Persists the mapping in the user record for restart recovery.
   */
  private updateDelegateMapping(ownerPubkey: string, applicationKeys: ApplicationKeys): void {
    const userRecord = this.getOrCreateUserRecord(ownerPubkey)
    const deviceIdentities = applicationKeys.getAllDevices()
      .map(d => d.identityPubkey)
      .filter(Boolean) as string[]

    // Update user record with known device identities
    userRecord.knownDeviceIdentities = deviceIdentities

    // Update in-memory mapping
    for (const identity of deviceIdentities) {
      this.delegateToOwner.set(identity, ownerPubkey)
    }

    // Persist
    this.storeUserRecord(ownerPubkey).catch(() => {})
  }

  private subscribeToUserApplicationKeys(
    pubkey: string,
    onApplicationKeys: (list: ApplicationKeys) => void
  ): Unsubscribe {
    return this.nostrSubscribe(
      {
        kinds: [APPLICATION_KEYS_EVENT_KIND],
        authors: [pubkey],
        "#d": ["double-ratchet/application-keys"],
      },
      (event) => {
        try {
          const list = ApplicationKeys.fromEvent(event)
          // Update delegate mapping whenever we receive an ApplicationKeys
          this.updateDelegateMapping(pubkey, list)
          onApplicationKeys(list)
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
      for (const cb of this.internalSubscriptions) cb(event, userPubkey)
      promoteToActive(session)
      this.storeUserRecord(userPubkey).catch(() => {})
    })
    this.storeUserRecord(userPubkey).catch(() => {})
    this.sessionSubscriptions.set(key, unsub)
  }

  private attachApplicationKeysSubscription(
    userPubkey: string,
    onApplicationKeys?: (applicationKeys: ApplicationKeys) => void | Promise<void>
  ): void {
    const key = `applicationkeys:${userPubkey}`
    if (this.inviteSubscriptions.has(key)) return

    const unsubscribe = this.subscribeToUserApplicationKeys(
      userPubkey,
      async (applicationKeys) => {
        if (onApplicationKeys) await onApplicationKeys(applicationKeys)
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
        .then(() => this.sendMessageHistory(userPubkey, device.identityPubkey))
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

    this.attachApplicationKeysSubscription(userPubkey, async (applicationKeys) => {
      const devices = applicationKeys.getAllDevices()
      const activeDeviceIds = new Set(devices.map(d => d.identityPubkey))

      // Handle devices no longer in list (revoked or ApplicationKeys recreated from scratch)
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

      // For each device in ApplicationKeys, subscribe to their Invite event
      for (const device of devices) {
        subscribeToDeviceInvite(device)
      }
    })
  }

  onEvent(callback: OnEventCallback) {
    this.internalSubscriptions.add(callback)

    return () => {
      this.internalSubscriptions.delete(callback)
    }
  }

  getDeviceId(): string {
    return this.deviceId
  }

  getUserRecords(): Map<string, UserRecord> {
    return this.userRecords
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

    const applicationKeysKey = `applicationkeys:${userPubkey}`
    const applicationKeysUnsub = this.inviteSubscriptions.get(applicationKeysKey)
    if (applicationKeysUnsub) {
      applicationKeysUnsub()
      this.inviteSubscriptions.delete(applicationKeysKey)
    }

    this.messageHistory.delete(userPubkey)

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

  private async sendMessageHistory(
    recipientPublicKey: string,
    deviceId: string
  ): Promise<void> {
    const history = this.messageHistory.get(recipientPublicKey) || []
    const userRecord = this.userRecords.get(recipientPublicKey)
    if (!userRecord) {
      return
    }
    const device = userRecord.devices.get(deviceId)
    if (!device) {
      return
    }
    for (const event of history) {
      const { activeSession } = device

      if (!activeSession) {
        continue
      }
      const { event: verifiedEvent } = activeSession.sendEvent(event)
      await this.nostrPublish(verifiedEvent)
      await this.storeUserRecord(recipientPublicKey)
    }
  }

  async sendEvent(
    recipientIdentityKey: string,
    event: Partial<Rumor>
  ): Promise<Rumor | undefined> {
    await this.init()

    // Add to message history queue (will be sent when session is established)
    const completeEvent = event as Rumor
    // Use ownerPublicKey for history targets so delegates share history with owner
    const historyTargets = new Set([recipientIdentityKey, this.ownerPublicKey])
    for (const key of historyTargets) {
      const existing = this.messageHistory.get(key) || []
      this.messageHistory.set(key, [...existing, completeEvent])
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

    // Send to all devices and await completion before returning
    // This ensures session state is ratcheted and persisted before function returns
    await Promise.allSettled(
      devices.map(async (device) => {
        const { activeSession } = device
        if (!activeSession) {
          return
        }
        const { event: verifiedEvent } = activeSession.sendEvent(event)
        await this.nostrPublish(verifiedEvent).catch(() => {})
      })
    )

    // Store recipient's user record after all messages sent
    await this.storeUserRecord(recipientIdentityKey)
    // Also store owner's record if different (for sibling device sessions)
    // This ensures session state is persisted after ratcheting for both:
    // - recipientDevices stored under recipientIdentityKey
    // - Own sibling devices stored under ownerPublicKey
    if (this.ownerPublicKey !== recipientIdentityKey) {
      await this.storeUserRecord(this.ownerPublicKey)
    }

    // Return the event with computed ID (same as library would compute)
    return completeEvent
  }

  async sendMessage(
    recipientPublicKey: string,
    content: string,
    options: { kind?: number; tags?: string[][] } = {}
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

    rumor.id = getEventHash(rumor)

    // Use sendEvent for actual sending (includes queueing)
    // Note: sendEvent is not awaited to maintain backward compatibility
    // The message is queued and will be sent when sessions are established
    this.sendEvent(recipientPublicKey, rumor).catch(() => {})

    return rumor
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
      knownDeviceIdentities: userRecord?.knownDeviceIdentities || [],
    }
    return this.storage.put(this.userRecordKey(publicKey), data)
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

        const knownDeviceIdentities = data.knownDeviceIdentities || []

        this.userRecords.set(publicKey, {
          publicKey: data.publicKey,
          devices,
          knownDeviceIdentities,
        })

        // Rebuild delegateToOwner mapping from stored device identities
        for (const identity of knownDeviceIdentities) {
          this.delegateToOwner.set(identity, publicKey)
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
