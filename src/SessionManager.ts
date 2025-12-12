import {
  DecryptFunction,
  NostrSubscribe,
  NostrPublish,
  Rumor,
  Unsubscribe,
  INVITE_EVENT_KIND,
  CHAT_MESSAGE_KIND,
} from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { Invite } from "./Invite"
import { Session } from "./Session"
import { serializeSessionState, deserializeSessionState } from "./utils"
import { getEventHash, VerifiedEvent } from "nostr-tools"

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

  // Data
  private userRecords: Map<string, UserRecord> = new Map()
  private messageHistory: Map<string, Rumor[]> = new Map()
  private currentDeviceInvite: Invite | null = null

  // Subscriptions
  private ourDeviceInviteSubscription: Unsubscribe | null = null
  private ourDeviceIntiveTombstoneSubscription: Unsubscribe | null = null
  private inviteSubscriptions: Map<string, Unsubscribe> = new Map()
  private sessionSubscriptions: Map<string, Unsubscribe> = new Map()
  private inviteTombstoneSubscriptions: Map<string, Unsubscribe> = new Map()

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
    storage?: StorageAdapter
  ) {
    this.userRecords = new Map()
    this.nostrSubscribe = nostrSubscribe
    this.nostrPublish = nostrPublish
    this.ourPublicKey = ourPublicKey
    this.ourIdentityKey = ourIdentityKey
    this.deviceId = deviceId
    this.storage = storage || new InMemoryStorageAdapter()
    this.versionPrefix = `v${this.storageVersion}`
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

    const ourInviteFromStorage: Invite | null = await this.storage
      .get<string>(this.deviceInviteKey(this.deviceId))
      .then((data) => {
        if (!data) return null
        try {
          return Invite.deserialize(data)
        } catch {
          return null
        }
      })

    const invite =
      ourInviteFromStorage || Invite.createNew(this.ourPublicKey, this.deviceId)

    this.currentDeviceInvite = invite

    await this.storage.put(this.deviceInviteKey(this.deviceId), invite.serialize())

    this.ourDeviceInviteSubscription = invite.listen(
      this.ourIdentityKey,
      this.nostrSubscribe,
      async (session, inviteePubkey, deviceId) => {
        if (!deviceId || deviceId === this.deviceId) return
        const nostrEventId = session.name
        const acceptanceKey = this.inviteAcceptKey(nostrEventId, inviteePubkey, deviceId)
        const nostrEventIdInStorage = await this.storage.get<string>(acceptanceKey)
        if (nostrEventIdInStorage) {
          return
        }

        await this.storage.put(acceptanceKey, "1")

        const userRecord = this.getOrCreateUserRecord(inviteePubkey)
        const deviceRecord = this.upsertDeviceRecord(userRecord, deviceId)

        this.attachSessionSubscription(inviteePubkey, deviceRecord, session, true)
      }
    )

    if (!this.ourDeviceIntiveTombstoneSubscription) {
      this.ourDeviceIntiveTombstoneSubscription = this.createInviteTombstoneSubscription(
        this.ourPublicKey
      )
    }

    const inviteNostrEvent = invite.getEvent()
    this.nostrPublish(inviteNostrEvent).catch((error) => {
      console.error("Failed to publish our device invite:", error)
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

  private createInviteTombstoneSubscription(authorPublicKey: string): Unsubscribe {
    return this.nostrSubscribe(
      {
        kinds: [INVITE_EVENT_KIND],
        authors: [authorPublicKey],
        "#l": ["double-ratchet/invites"],
      },
      (event: VerifiedEvent) => {
        try {
          const isTombstone = !event.tags?.some(
            ([key]) => key === "ephemeralKey" || key === "sharedSecret"
          )
          if (isTombstone) {
            const deviceIdTag = event.tags.find(
              ([key, value]) => key === "d" && value.startsWith("double-ratchet/invites/")
            )
            const [, deviceIdTagValue] = deviceIdTag || []
            const deviceId = deviceIdTagValue.split("/").pop()
            if (!deviceId) return

            this.cleanupDevice(authorPublicKey, deviceId)
          }
        } catch (error) {
          console.error("Failed to handle device tombstone:", error)
        }
      }
    )
  }

  private sessionKey(userPubkey: string, deviceId: string, sessionName: string) {
    return `${this.sessionKeyPrefix(userPubkey)}${deviceId}/${sessionName}`
  }
  private inviteKey(userPubkey: string) {
    return this.userInviteKey(userPubkey)
  }
  private inviteAcceptKey(nostrEventId: string, userPubkey: string, deviceId: string) {
    return `${this.inviteAcceptKeyPrefix(userPubkey)}${deviceId}/${nostrEventId}`
  }

  private deviceInviteKey(deviceId: string) {
    return `${this.versionPrefix}/device-invite/${deviceId}`
  }

  private userInviteKey(userPubkey: string) {
    return `${this.versionPrefix}/invite/${userPubkey}`
  }

  private inviteAcceptKeyPrefix(userPublicKey: string) {
    return `${this.versionPrefix}/invite-accept/${userPublicKey}/`
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

  private attachInviteTombstoneSubscription(userPubkey: string): void {
    if (this.inviteTombstoneSubscriptions.has(userPubkey)) {
      return
    }

    const unsubscribe = this.createInviteTombstoneSubscription(userPubkey)
    this.inviteTombstoneSubscriptions.set(userPubkey, unsubscribe)
  }

  setupUser(userPubkey: string) {
    const userRecord = this.getOrCreateUserRecord(userPubkey)

    this.attachInviteTombstoneSubscription(userPubkey)

    const acceptInvite = async (invite: Invite) => {
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

    this.attachInviteSubscription(userPubkey, async (invite) => {
      const { deviceId } = invite
      if (!deviceId) return

      if (!userRecord.devices.has(deviceId)) {
        await acceptInvite(invite)
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

  getDeviceInviteEphemeralKey(): string | null {
    return this.currentDeviceInvite?.inviterEphemeralPublicKey || null
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

    for (const unsubscribe of this.inviteTombstoneSubscriptions.values()) {
      unsubscribe()
    }

    this.ourDeviceInviteSubscription?.()
    this.ourDeviceIntiveTombstoneSubscription?.()
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

    const tombstoneUnsub = this.inviteTombstoneSubscriptions.get(userPubkey)
    if (tombstoneUnsub) {
      tombstoneUnsub()
      this.inviteTombstoneSubscriptions.delete(userPubkey)
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

    const devices = [
      ...Array.from(userRecord.devices.values()),
      ...Array.from(ourUserRecord.devices.values()),
    ].filter((device) => device.staleAt === undefined)

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

  async revokeDevice(deviceId: string): Promise<void> {
    await this.init()

    await this.publishDeviceTombstone(deviceId).catch((error) => {
      console.error("Failed to publish device tombstone:", error)
    })

    await this.cleanupDevice(this.ourPublicKey, deviceId)
  }

  private async publishDeviceTombstone(deviceId: string): Promise<void> {
    const tags: string[][] = [
      ["l", "double-ratchet/invites"],
      ["d", `double-ratchet/invites/${deviceId}`],
    ]

    const deletionEvent = {
      content: "",
      kind: INVITE_EVENT_KIND,
      created_at: Math.floor(Date.now() / 1000),
      tags,
      pubkey: this.ourPublicKey,
    }

    await this.nostrPublish(deletionEvent)
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

        if (publicKey !== this.ourPublicKey) {
          this.attachInviteTombstoneSubscription(publicKey)
        }

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

      // Set version to 1 so next migration can run
      version = "1"
      await this.storage.put(this.versionKey(), version)

      return
    }

    // Future migrations
    if (version === "1") {
      return
    }
  }
}
