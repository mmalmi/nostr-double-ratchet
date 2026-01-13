import {
  DecryptFunction,
  NostrSubscribe,
  NostrPublish,
  Rumor,
  Unsubscribe,
  INVITE_LIST_EVENT_KIND,
  CHAT_MESSAGE_KIND,
} from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { Invite } from "./Invite"
import { InviteList } from "./InviteList"
import { Session } from "./Session"
import { serializeSessionState, deserializeSessionState } from "./utils"
import { getEventHash } from "nostr-tools"

export type OnEventCallback = (event: Rumor, from: string) => void

interface DeviceRecord {
  deviceId: string
  activeSession?: Session
  inactiveSessions: Session[]
  createdAt: number
  staleAt?: number
}

interface UserRecord {
  publicKey: string
  devices: Map<string, DeviceRecord>
}

type StoredSessionEntry = ReturnType<typeof serializeSessionState>

interface StoredDeviceRecord {
  deviceId: string
  activeSession: StoredSessionEntry | null
  inactiveSessions: StoredSessionEntry[]
  createdAt: number
  staleAt?: number
}

interface StoredUserRecord {
  publicKey: string
  devices: StoredDeviceRecord[]
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
  private ourIdentityKey: Uint8Array | DecryptFunction
  private ourPublicKey: string

  // Ephemeral keypair for listening to invite responses (optional)
  private ephemeralKeypair?: { publicKey: string; privateKey: Uint8Array }
  private sharedSecret?: string

  // Data
  private userRecords: Map<string, UserRecord> = new Map()
  private messageHistory: Map<string, Rumor[]> = new Map()
  private userInviteLists: Map<string, InviteList> = new Map()
  // Map delegate device pubkeys to their owner's pubkey
  private delegateToOwner: Map<string, string> = new Map()

  // Subscriptions
  private ourDeviceInviteSubscription: Unsubscribe | null = null
  private ourInviteResponseSubscription: Unsubscribe | null = null
  private inviteSubscriptions: Map<string, Unsubscribe> = new Map()
  private sessionSubscriptions: Map<string, Unsubscribe> = new Map()

  // Callbacks
  private internalSubscriptions: Set<OnEventCallback> = new Set()

  // Initialization flag
  private initialized: boolean = false

  constructor(
    ourPublicKey: string,
    ourIdentityKey: Uint8Array | DecryptFunction,
    deviceId: string,
    nostrSubscribe: NostrSubscribe,
    nostrPublish: NostrPublish,
    storage?: StorageAdapter,
    ephemeralKeypair?: { publicKey: string; privateKey: Uint8Array },
    sharedSecret?: string
  ) {
    this.userRecords = new Map()
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.ourPublicKey = ourPublicKey
    this.ourIdentityKey = ourIdentityKey
    this.deviceId = deviceId
    this.storage = storage || new InMemoryStorageAdapter()
    this.versionPrefix = `v${this.storageVersion}`
    this.ephemeralKeypair = ephemeralKeypair
    this.sharedSecret = sharedSecret
  }

  /**
   * Set ephemeral keys after construction (for async initialization)
   */
  setEphemeralKeys(
    ephemeralKeypair: { publicKey: string; privateKey: Uint8Array },
    sharedSecret: string
  ): void {
    this.ephemeralKeypair = ephemeralKeypair
    this.sharedSecret = sharedSecret

    // If already initialized, start the listener and setup own devices now
    if (this.initialized && !this.ourInviteResponseSubscription) {
      this.startInviteResponseListener()
      // Now that we can receive responses, setup sessions with our own other devices
      this.setupUser(this.ourPublicKey)
    }
  }

  async init() {
    if (this.initialized) return
    this.initialized = true

    await this.runMigrations().catch((error) => {
      console.error("Failed to run migrations:", error)
    })

    await this.loadAllUserRecords().catch((error) => {
      console.error("Failed to load user records:", error)
    })

    // Add our own device to user record to prevent accepting our own invite
    const ourUserRecord = this.getOrCreateUserRecord(this.ourPublicKey)
    this.upsertDeviceRecord(ourUserRecord, this.deviceId)

    // IMPORTANT: Start invite response listener BEFORE setting up users
    // This ensures we're listening when other devices respond to our invites
    if (this.ephemeralKeypair && this.sharedSecret) {
      this.startInviteResponseListener()
      // Setup sessions with our own other devices (only if we can receive responses)
      this.setupUser(this.ourPublicKey)
    }
    // If no ephemeral keys yet, setupUser will be called when setEphemeralKeys is called
  }

  /**
   * Start listening for invite responses on our ephemeral key.
   * This is used by devices to receive session establishment responses.
   */
  private startInviteResponseListener(): void {
    if (!this.ephemeralKeypair || !this.sharedSecret) return

    const { publicKey: ephemeralPubkey, privateKey: ephemeralPrivkey } = this.ephemeralKeypair
    const sharedSecret = this.sharedSecret

    // Subscribe to invite responses tagged to our ephemeral key
    this.ourInviteResponseSubscription = this.nostrSubscribe(
      {
        kinds: [1059], // INVITE_RESPONSE_KIND
        "#p": [ephemeralPubkey],
      },
      async (event) => {
        try {
          const { decryptInviteResponse, createSessionFromAccept } = await import("./inviteUtils")

          const decrypted = await decryptInviteResponse({
            envelopeContent: event.content,
            envelopeSenderPubkey: event.pubkey,
            inviterEphemeralPrivateKey: ephemeralPrivkey,
            inviterPrivateKey: this.ourIdentityKey instanceof Uint8Array ? this.ourIdentityKey : undefined,
            sharedSecret,
            decrypt: typeof this.ourIdentityKey === "function" ? this.ourIdentityKey : undefined,
          })

          const session = createSessionFromAccept({
            nostrSubscribe: this.nostrSubscribe,
            theirPublicKey: decrypted.inviteeSessionPublicKey,
            ourSessionPrivateKey: ephemeralPrivkey,
            sharedSecret,
            isSender: false,
            name: event.id,
          })

          // Resolve delegate pubkey to owner for correct UserRecord attribution
          const ownerPubkey = this.resolveToOwner(decrypted.inviteeIdentity)
          const userRecord = this.getOrCreateUserRecord(ownerPubkey)
          const deviceRecord = this.upsertDeviceRecord(userRecord, decrypted.deviceId || "default")

          this.attachSessionSubscription(ownerPubkey, deviceRecord, session, true)
        } catch {
          // Invalid response, ignore
        }
      }
    )
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
  private inviteKey(userPubkey: string) {
    return this.userInviteKey(userPubkey)
  }
  private userInviteKey(userPubkey: string) {
    return `${this.versionPrefix}/invite/${userPubkey}`
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
  resolveToOwner(pubkey: string): string {
    return this.delegateToOwner.get(pubkey) || pubkey
  }

  /**
   * Update the delegate-to-owner mapping from an InviteList.
   * Extracts delegate device pubkeys and maps them to the owner.
   */
  private updateDelegateMapping(ownerPubkey: string, inviteList: InviteList): void {
    for (const device of inviteList.getAllDevices()) {
      if (device.identityPubkey) {
        this.delegateToOwner.set(device.identityPubkey, ownerPubkey)
      }
    }
  }

  private subscribeToUserInviteList(
    pubkey: string,
    onInviteList: (list: InviteList) => void
  ): Unsubscribe {
    return this.nostrSubscribe(
      {
        kinds: [INVITE_LIST_EVENT_KIND],
        authors: [pubkey],
        "#d": ["double-ratchet/invite-list"],
      },
      (event) => {
        try {
          const list = InviteList.fromEvent(event)
          // Update delegate mapping whenever we receive an InviteList
          this.updateDelegateMapping(pubkey, list)
          onInviteList(list)
        } catch {
          // Invalid event, ignore
        }
      }
    )
  }

  private attachSessionSubscription(
    userPubkey: string,
    deviceRecord: DeviceRecord,
    session: Session,
    // Set to true if only handshake -> not yet sendable -> will be promoted on message
    inactive: boolean = false
  ): void {
    if (deviceRecord.staleAt !== undefined) return

    const key = this.sessionKey(userPubkey, deviceRecord.deviceId, session.name)
    if (this.sessionSubscriptions.has(key)) return

    const dr = deviceRecord
    const rotateSession = (nextSession: Session) => {
      const current = dr.activeSession

      if (!current) {
        dr.activeSession = nextSession
        return
      }

      if (current === nextSession || current.name === nextSession.name) {
        dr.activeSession = nextSession
        return
      }

      dr.inactiveSessions = dr.inactiveSessions.filter(
        (session) => session !== current && session.name !== current.name
      )

      dr.inactiveSessions.push(current)
      dr.inactiveSessions = dr.inactiveSessions.slice(-1)
      dr.activeSession = nextSession
    }

    if (inactive) {
      const alreadyTracked = dr.inactiveSessions.some(
        (tracked) => tracked === session || tracked.name === session.name
      )
      if (!alreadyTracked) {
        dr.inactiveSessions.push(session)
        dr.inactiveSessions = dr.inactiveSessions.slice(-1)
      }
    } else {
      rotateSession(session)
    }

    const unsub = session.onEvent((event) => {
      for (const cb of this.internalSubscriptions) cb(event, userPubkey)
      rotateSession(session)
      this.storeUserRecord(userPubkey).catch(console.error)
    })
    this.storeUserRecord(userPubkey).catch(console.error)
    this.sessionSubscriptions.set(key, unsub)
  }

  private attachInviteSubscription(
    userPubkey: string,
    onInvite?: (invite: Invite) => void | Promise<void>
  ): void {
    const key = this.inviteKey(userPubkey)
    if (this.inviteSubscriptions.has(key)) return

    const unsubscribe = Invite.fromUser(
      userPubkey,
      this.nostrSubscribe,
      async (invite) => {
        if (!invite.deviceId) return
        if (onInvite) await onInvite(invite)
      }
    )

    this.inviteSubscriptions.set(key, unsubscribe)
  }

  private attachInviteListSubscription(
    userPubkey: string,
    onInviteList?: (inviteList: InviteList) => void | Promise<void>
  ): void {
    const key = `invitelist:${userPubkey}`
    if (this.inviteSubscriptions.has(key)) return

    const unsubscribe = this.subscribeToUserInviteList(
      userPubkey,
      async (inviteList) => {
        // Cache the InviteList - if user has one, we use it as source of truth
        this.userInviteLists.set(userPubkey, inviteList)
        if (onInviteList) await onInviteList(inviteList)
      }
    )

    this.inviteSubscriptions.set(key, unsubscribe)
  }

  setupUser(userPubkey: string) {
    const userRecord = this.getOrCreateUserRecord(userPubkey)

    // Helper to accept an invite (works for both InviteList devices and legacy Invite)
    const acceptInviteFromDevice = async (
      inviteList: InviteList,
      deviceId: string
    ) => {
      const { session, event } = await inviteList.accept(
        deviceId,
        this.nostrSubscribe,
        this.ourPublicKey,
        this.ourIdentityKey,
        this.deviceId
      )
      return this.nostrPublish(event)
        .then(() => this.upsertDeviceRecord(userRecord, deviceId))
        .then((dr) => this.attachSessionSubscription(userPubkey, dr, session))
        .then(() => this.sendMessageHistory(userPubkey, deviceId))
        .catch(console.error)
    }

    const acceptLegacyInvite = async (invite: Invite) => {
      const { deviceId } = invite
      if (!deviceId) return

      const { session, event } = await invite.accept(
        this.nostrSubscribe,
        this.ourPublicKey,
        this.ourIdentityKey,
        this.deviceId
      )
      return this.nostrPublish(event)
        .then(() => this.upsertDeviceRecord(userRecord, deviceId))
        .then((dr) => this.attachSessionSubscription(userPubkey, dr, session))
        .then(() => this.sendMessageHistory(userPubkey, deviceId))
        .catch(console.error)
    }

    // Subscribe to InviteList (kind 10078) - new format
    this.attachInviteListSubscription(userPubkey, async (inviteList) => {
      // Handle removed devices (source of truth for revocation)
      for (const deviceId of inviteList.getRemovedDeviceIds()) {
        await this.cleanupDevice(userPubkey, deviceId)
      }

      // Accept invites from new devices
      for (const device of inviteList.getAllDevices()) {
        if (!userRecord.devices.has(device.deviceId)) {
          await acceptInviteFromDevice(inviteList, device.deviceId)
        }
      }
    })

    // Subscribe to per-device invites (kind 30078) - legacy format for backwards compatibility
    this.attachInviteSubscription(userPubkey, async (invite) => {
      const { deviceId } = invite
      if (!deviceId) return

      if (!userRecord.devices.has(deviceId)) {
        await acceptLegacyInvite(invite)
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

    this.ourDeviceInviteSubscription?.()
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
    this.storeUserRecord(publicKey).catch(console.error)
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

    const inviteKey = this.inviteKey(userPubkey)
    const inviteUnsub = this.inviteSubscriptions.get(inviteKey)
    if (inviteUnsub) {
      inviteUnsub()
      this.inviteSubscriptions.delete(inviteKey)
    }

    this.messageHistory.delete(userPubkey)

    await Promise.allSettled([
      this.storage.del(this.inviteKey(userPubkey)),
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
    if (device.staleAt !== undefined) {
      return
    }
    for (const event of history) {
      const { activeSession } = device

      if (!activeSession) continue
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
    const historyTargets = new Set([recipientIdentityKey, this.ourPublicKey])
    for (const key of historyTargets) {
      const existing = this.messageHistory.get(key) || []
      this.messageHistory.set(key, [...existing, completeEvent])
    }

    const userRecord = this.getOrCreateUserRecord(recipientIdentityKey)
    const ourUserRecord = this.getOrCreateUserRecord(this.ourPublicKey)

    this.setupUser(recipientIdentityKey)
    this.setupUser(this.ourPublicKey)

    const recipientDevices = Array.from(userRecord.devices.values()).filter(d => d.staleAt === undefined)
    const ownDevices = Array.from(ourUserRecord.devices.values()).filter(d => d.staleAt === undefined)
    const devices = [...recipientDevices, ...ownDevices]

    // Send to all devices in background (if sessions exist)
    Promise.allSettled(
      devices.map(async (device) => {
        const { activeSession } = device
        if (!activeSession) return
        const { event: verifiedEvent } = activeSession.sendEvent(event)
        await this.nostrPublish(verifiedEvent).catch(console.error)
      })
    )
      .then(() => {
        this.storeUserRecord(recipientIdentityKey)
      })
      .catch(console.error)

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
    this.sendEvent(recipientPublicKey, rumor).catch(console.error)

    return rumor
  }

  private async cleanupDevice(publicKey: string, deviceId: string): Promise<void> {
    const userRecord = this.userRecords.get(publicKey)
    if (!userRecord) return
    const deviceRecord = userRecord.devices.get(deviceId)

    if (!deviceRecord) return

    if (deviceRecord.activeSession) {
      this.removeSessionSubscription(publicKey, deviceId, deviceRecord.activeSession.name)
    }

    for (const session of deviceRecord.inactiveSessions) {
      this.removeSessionSubscription(publicKey, deviceId, session.name)
    }

    deviceRecord.activeSession = undefined
    deviceRecord.inactiveSessions = []
    deviceRecord.staleAt = Date.now()

    await this.storeUserRecord(publicKey).catch(console.error)
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
    const data: StoredUserRecord = {
      publicKey: publicKey,
      devices: Array.from(this.userRecords.get(publicKey)?.devices.entries() || []).map(
        ([, device]) => ({
          deviceId: device.deviceId,
          activeSession: device.activeSession
            ? serializeSessionState(device.activeSession.state)
            : null,
          inactiveSessions: device.inactiveSessions.map((session) =>
            serializeSessionState(session.state)
          ),
          createdAt: device.createdAt,
          staleAt: device.staleAt,
        })
      ),
    }
    return this.storage.put(this.userRecordKey(publicKey), data)
  }

  private loadUserRecord(publicKey: string) {
    return this.storage
      .get<StoredUserRecord>(this.userRecordKey(publicKey))
      .then((data) => {
        if (!data) return

        const devices = new Map<string, DeviceRecord>()

        for (const deviceData of data.devices) {
          const {
            deviceId,
            activeSession: serializedActive,
            inactiveSessions: serializedInactive,
            createdAt,
            staleAt,
          } = deviceData

          try {
            const activeSession = serializedActive
              ? new Session(
                  this.nostrSubscribe,
                  deserializeSessionState(serializedActive)
                )
              : undefined

            const inactiveSessions = serializedInactive.map(
              (entry) => new Session(this.nostrSubscribe, deserializeSessionState(entry))
            )

            devices.set(deviceId, {
              deviceId,
              activeSession,
              inactiveSessions,
              createdAt,
              staleAt,
            })
          } catch (e) {
            console.error(
              `Failed to deserialize session for user ${publicKey}, device ${deviceId}:`,
              e
            )
          }
        }

        this.userRecords.set(publicKey, {
          publicKey: data.publicKey,
          devices,
        })

        for (const device of devices.values()) {
          const { deviceId, activeSession, inactiveSessions, staleAt } = device
          if (!deviceId || staleAt !== undefined) continue

          for (const session of inactiveSessions.reverse()) {
            this.attachSessionSubscription(publicKey, device, session)
          }
          if (activeSession) {
            this.attachSessionSubscription(publicKey, device, activeSession)
          }
        }
      })
      .catch((error) => {
        console.error(`Failed to load user record for ${publicKey}:`, error)
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
      // Fetch all existing invites
      // Assume no version prefix
      // Deserialize and serialize to start using persistent createdAt
      // Re-save invites with proper keys
      const oldInvitePrefix = "invite/"
      const inviteKeys = await this.storage.list(oldInvitePrefix)
      await Promise.all(
        inviteKeys.map(async (key) => {
          try {
            const publicKey = key.slice(oldInvitePrefix.length)
            const inviteData = await this.storage.get<string>(key)
            if (inviteData) {
              const newKey = this.userInviteKey(publicKey)
              const invite = Invite.deserialize(inviteData)
              const serializedInvite = invite.serialize()
              await this.storage.put(newKey, serializedInvite)
              await this.storage.del(key)
            }
          } catch (e) {
            console.error("Migration error for invite:", e)
          }
        })
      )

      // Fetch all existing user records
      // Assume no version prefix
      // Remove all old sessions as these may have key issues
      // Re-save user records without sessions with proper keys
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
          } catch (e) {
            console.error("Migration error for user record:", e)
          }
        })
      )

      // Set version to 1
      version = "1"
      await this.storage.put(this.versionKey(), version)
    }
  }
}
