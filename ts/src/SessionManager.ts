import {
  IdentityKey,
  NostrSubscribe,
  NostrPublish,
  Rumor,
  Unsubscribe,
  INVITE_LIST_EVENT_KIND,
  CHAT_MESSAGE_KIND,
} from "./types"
import { StorageAdapter, InMemoryStorageAdapter } from "./StorageAdapter"
import { InviteList, DeviceEntry } from "./InviteList"
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
  staleAt?: number
  // Set to true when we've processed an invite response from this device
  // This survives restarts and prevents duplicate RESPONDER session creation
  hasResponderSession?: boolean
}

export interface UserRecord {
  publicKey: string
  devices: Map<string, DeviceRecord>
  /** Device identity pubkeys from InviteList - used to rebuild delegateToOwner on load */
  knownDeviceIdentities: string[]
}

type StoredSessionEntry = ReturnType<typeof serializeSessionState>

interface StoredDeviceRecord {
  deviceId: string
  activeSession: StoredSessionEntry | null
  inactiveSessions: StoredSessionEntry[]
  createdAt: number
  staleAt?: number
  hasResponderSession?: boolean
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

    await this.runMigrations().catch((error) => {
      console.error("Failed to run migrations:", error)
    })

    await this.loadAllUserRecords().catch((error) => {
      console.error("Failed to load user records:", error)
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

          console.log(`[InviteResponse] Received from device=${decrypted.inviteeIdentity.slice(0,8)}, ownerPublicKey=${decrypted.ownerPublicKey?.slice(0,8) || 'none'}, sessionKey=${decrypted.inviteeSessionPublicKey.slice(0,8)}`)

          // Get owner pubkey from response (required for proper chat routing)
          // If not present (old client), fall back to resolveToOwner
          const claimedOwner = decrypted.ownerPublicKey || this.resolveToOwner(decrypted.inviteeIdentity)
          console.log(`[InviteResponse] claimedOwner=${claimedOwner.slice(0,8)}, ourDevice=${this.deviceId.slice(0,8)}, ourOwner=${this.ownerPublicKey.slice(0,8)}`)

          // Verify the device is authorized by fetching owner's InviteList
          console.log(`[InviteResponse] Fetching InviteList for owner=${claimedOwner.slice(0,8)}...`)
          const inviteList = await this.fetchInviteList(claimedOwner)
          console.log(`[InviteResponse] InviteList fetch result: ${inviteList ? `found with ${inviteList.getAllDevices().length} devices` : 'NOT FOUND'}`)

          if (!inviteList) {
            // No InviteList found - check cached device identities as fallback
            const cachedRecord = this.userRecords.get(claimedOwner)
            const cachedIdentities = cachedRecord?.knownDeviceIdentities || []
            console.log(`[InviteResponse] No InviteList, checking cache: cachedIdentities=[${cachedIdentities.map(i => i.slice(0,8)).join(', ')}]`)

            if (cachedIdentities.includes(decrypted.inviteeIdentity)) {
              // Device is in cached list - allow (this handles restart scenarios)
              console.log(`[InviteResponse] ALLOWED: device found in cached identities`)
            } else if (decrypted.inviteeIdentity === claimedOwner) {
              // Single-device user (device = owner), proceed without InviteList
              console.log(`[InviteResponse] ALLOWED: single-device user (device === owner)`)
            } else {
              console.warn(`[InviteResponse] REJECTED: no InviteList found for claimed owner ${claimedOwner.slice(0,8)} and device ${decrypted.inviteeIdentity.slice(0,8)} is not the owner or cached`)
              return
            }
          } else {
            // Check that the responding device is actually in the owner's InviteList
            const deviceInList = inviteList.getAllDevices().some(
              d => d.identityPubkey === decrypted.inviteeIdentity
            )
            console.log(`[InviteResponse] InviteList devices: [${inviteList.getAllDevices().map(d => d.identityPubkey.slice(0,8)).join(', ')}]`)
            if (!deviceInList) {
              console.warn(`[InviteResponse] REJECTED: device ${decrypted.inviteeIdentity.slice(0,8)} not in owner's InviteList`)
              return
            }
            console.log(`[InviteResponse] ALLOWED: device found in InviteList`)

            // Update delegate mapping with verified InviteList
            this.updateDelegateMapping(claimedOwner, inviteList)
          }

          const ownerPubkey = claimedOwner
          const userRecord = this.getOrCreateUserRecord(ownerPubkey)
          // inviteeIdentity serves as the device ID
          const deviceRecord = this.upsertDeviceRecord(userRecord, decrypted.inviteeIdentity)

          // Check for duplicate/stale responses using the persisted flag
          // This flag survives restarts and prevents creating duplicate RESPONDER sessions
          if (deviceRecord.hasResponderSession) {
            return
          }

          // Also check session state as a fallback (for existing sessions before the flag was added)
          const responseSessionKey = decrypted.inviteeSessionPublicKey
          const existingSession = deviceRecord.activeSession
          const existingInactive = deviceRecord.inactiveSessions || []
          const allSessions = existingSession ? [existingSession, ...existingInactive] : existingInactive

          // Check if any existing session can already receive from this device:
          // - Has receivingChainKey set (RESPONDER session has received messages)
          // - Has the same theirNextNostrPublicKey (same session, duplicate response)
          const canAlreadyReceive = allSessions.some(s =>
            s.state?.receivingChainKey !== undefined ||
            s.state?.theirNextNostrPublicKey === responseSessionKey ||
            s.state?.theirCurrentNostrPublicKey === responseSessionKey
          )
          if (canAlreadyReceive) {
            return
          }

          const session = createSessionFromAccept({
            nostrSubscribe: this.nostrSubscribe,
            theirPublicKey: decrypted.inviteeSessionPublicKey,
            ourSessionPrivateKey: ephemeralPrivkey,
            sharedSecret,
            isSender: false,
            name: event.id,
          })

          // Mark that we've processed a responder session for this device
          // This flag is persisted and survives restarts
          deviceRecord.hasResponderSession = true

          this.attachSessionSubscription(ownerPubkey, deviceRecord, session, true)
          console.log(`[InviteResponse] SUCCESS: Created RESPONDER session for owner=${ownerPubkey.slice(0,8)}, device=${decrypted.inviteeIdentity.slice(0,8)}, sessionName=${session.name.slice(0,8)}`)
          // Persist the flag
          this.storeUserRecord(ownerPubkey).catch(console.error)
        } catch (err) {
          console.error(`[InviteResponse] ERROR decrypting invite response:`, err)
        }
      }
    )
  }

  /**
   * Fetch a user's InviteList from relays.
   * Returns null if not found within timeout.
   */
  private fetchInviteList(pubkey: string, timeoutMs = 2000): Promise<InviteList | null> {
    return new Promise((resolve) => {
      let latestEvent: { created_at: number; inviteList: InviteList } | null = null
      let resolved = false

      // Use a short initial delay before resolving to allow event delivery
      const resolveResult = () => {
        if (resolved) return
        resolved = true
        unsubscribe()
        resolve(latestEvent?.inviteList ?? null)
      }

      // Start timeout
      const timeout = setTimeout(resolveResult, timeoutMs)

      const unsubscribe = this.nostrSubscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          authors: [pubkey],
          "#d": ["double-ratchet/invite-list"],
        },
        (event) => {
          if (resolved) return
          try {
            const inviteList = InviteList.fromEvent(event)
            if (!latestEvent || event.created_at > latestEvent.created_at) {
              latestEvent = { created_at: event.created_at, inviteList }
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
   * Update the delegate-to-owner mapping from an InviteList.
   * Extracts delegate device pubkeys and maps them to the owner.
   * Persists the mapping in the user record for restart recovery.
   */
  private updateDelegateMapping(ownerPubkey: string, inviteList: InviteList): void {
    const userRecord = this.getOrCreateUserRecord(ownerPubkey)
    const deviceIdentities = inviteList.getAllDevices()
      .map(d => d.identityPubkey)
      .filter(Boolean) as string[]

    // Update user record with known device identities
    userRecord.knownDeviceIdentities = deviceIdentities

    // Update in-memory mapping
    for (const identity of deviceIdentities) {
      this.delegateToOwner.set(identity, ownerPubkey)
    }

    // Persist
    this.storeUserRecord(ownerPubkey).catch(console.error)
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
    if (deviceRecord.staleAt !== undefined) {
      return
    }

    const key = this.sessionKey(userPubkey, deviceRecord.deviceId, session.name)
    if (this.sessionSubscriptions.has(key)) {
      return
    }

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

  private attachInviteListSubscription(
    userPubkey: string,
    onInviteList?: (inviteList: InviteList) => void | Promise<void>
  ): void {
    const key = `invitelist:${userPubkey}`
    if (this.inviteSubscriptions.has(key)) return

    const unsubscribe = this.subscribeToUserInviteList(
      userPubkey,
      async (inviteList) => {
        if (onInviteList) await onInviteList(inviteList)
      }
    )

    this.inviteSubscriptions.set(key, unsubscribe)
  }

  setupUser(userPubkey: string) {
    const userRecord = this.getOrCreateUserRecord(userPubkey)

    // Track which device identities we've subscribed to for invites
    const subscribedDeviceIdentities = new Set<string>()

    /**
     * Accept an invite from a device.
     * The invite is fetched separately from the device's own Invite event.
     */
    const acceptInviteFromDevice = async (
      device: DeviceEntry,
      invite: Invite
    ) => {
      console.log(`[AcceptInvite] Accepting invite from user=${userPubkey.slice(0,8)}, device=${device.identityPubkey.slice(0,8)}`)
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
      console.log(`[AcceptInvite] Sending invite response: ourDevice=${this.ourPublicKey.slice(0,8)}, ourOwner=${this.ownerPublicKey.slice(0,8)}, sessionName=${session.name.slice(0,8)}`)
      return this.nostrPublish(event)
        .then(() => {
          this.attachSessionSubscription(userPubkey, deviceRecord, session)
          console.log(`[AcceptInvite] SUCCESS: Created INITIATOR session for user=${userPubkey.slice(0,8)}, device=${device.identityPubkey.slice(0,8)}`)
        })
        .then(() => this.sendMessageHistory(userPubkey, device.identityPubkey))
        .catch((err) => console.error(`[AcceptInvite] ERROR:`, err))
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

      // Already have a record for this device? Skip.
      if (userRecord.devices.has(device.identityPubkey)) {
        return
      }

      const inviteSubKey = `invite:${device.identityPubkey}`
      if (this.inviteSubscriptions.has(inviteSubKey)) {
        return
      }

      // Subscribe to this device's Invite event
      const unsub = Invite.fromUser(device.identityPubkey, this.nostrSubscribe, async (invite) => {
        console.log(`[SetupUser] Received Invite from device=${device.identityPubkey.slice(0,8)}, inviteDeviceId=${invite.deviceId?.slice(0,8)}`)
        // Verify the invite is for this device (identityPubkey is the device identifier)
        if (invite.deviceId !== device.identityPubkey) {
          console.log(`[SetupUser] SKIP: Invite deviceId mismatch (expected ${device.identityPubkey.slice(0,8)}, got ${invite.deviceId?.slice(0,8)})`)
          return
        }

        // Skip if we already have a device record (race condition guard)
        if (userRecord.devices.has(device.identityPubkey)) {
          console.log(`[SetupUser] SKIP: Already have device record for ${device.identityPubkey.slice(0,8)}`)
          return
        }

        await acceptInviteFromDevice(device, invite)
      })

      this.inviteSubscriptions.set(inviteSubKey, unsub)
    }

    this.attachInviteListSubscription(userPubkey, async (inviteList) => {
      const devices = inviteList.getAllDevices()
      const activeDeviceIds = new Set(devices.map(d => d.identityPubkey))
      console.log(`[SetupUser] Received InviteList for user=${userPubkey.slice(0,8)}, devices=[${devices.map(d => d.identityPubkey.slice(0,8)).join(', ')}]`)

      // Handle devices no longer in list (revoked or InviteList recreated from scratch)
      const userRecord = this.userRecords.get(userPubkey)
      if (userRecord) {
        for (const [deviceId, device] of userRecord.devices) {
          if (!activeDeviceIds.has(deviceId) && device.staleAt === undefined) {
            console.log(`[SetupUser] Device ${deviceId.slice(0,8)} no longer in InviteList, cleaning up`)
            await this.cleanupDevice(userPubkey, deviceId)
          }
        }
      }

      // For each device in InviteList, subscribe to their Invite event
      for (const device of devices) {
        console.log(`[SetupUser] Subscribing to Invite for device=${device.identityPubkey.slice(0,8)} of user=${userPubkey.slice(0,8)}`)
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

    const inviteListKey = `invitelist:${userPubkey}`
    const inviteListUnsub = this.inviteSubscriptions.get(inviteListKey)
    if (inviteListUnsub) {
      inviteListUnsub()
      this.inviteSubscriptions.delete(inviteListKey)
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
    console.log(`[SendMessageHistory] recipient=${recipientPublicKey.slice(0,8)}, device=${deviceId.slice(0,8)}, historyCount=${history.length}`)
    const userRecord = this.userRecords.get(recipientPublicKey)
    if (!userRecord) {
      console.log(`[SendMessageHistory] No userRecord for recipient`)
      return
    }
    const device = userRecord.devices.get(deviceId)
    if (!device) {
      console.log(`[SendMessageHistory] No device record for deviceId`)
      return
    }
    if (device.staleAt !== undefined) {
      console.log(`[SendMessageHistory] Device is stale`)
      return
    }
    for (const event of history) {
      const { activeSession } = device

      if (!activeSession) {
        console.log(`[SendMessageHistory] No activeSession for device`)
        continue
      }
      console.log(`[SendMessageHistory] Sending queued message to device=${deviceId.slice(0,8)}`)
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

    const recipientDevices = Array.from(userRecord.devices.values()).filter(d => d.staleAt === undefined)
    const ownDevices = Array.from(ourUserRecord.devices.values()).filter(d => d.staleAt === undefined)

    // Merge and deduplicate by deviceId, excluding our own sending device
    // This fixes the self-message bug where sending to yourself would duplicate devices
    const deviceMap = new Map<string, DeviceRecord>()
    for (const d of [...recipientDevices, ...ownDevices]) {
      if (d.deviceId !== this.deviceId) {  // Exclude sender's own device
        deviceMap.set(d.deviceId, d)
      }
    }
    const devices = Array.from(deviceMap.values())

    console.log(`[SendEvent] to recipient=${recipientIdentityKey.slice(0,8)}, recipientDevices=[${recipientDevices.map(d => d.deviceId.slice(0,8) + (d.activeSession ? '✓' : '✗')).join(', ')}], ownDevices=[${ownDevices.map(d => d.deviceId.slice(0,8) + (d.activeSession ? '✓' : '✗')).join(', ')}]`)
    console.log(`[SendEvent] Final target devices (excl self ${this.deviceId.slice(0,8)}): [${devices.map(d => d.deviceId.slice(0,8) + (d.activeSession ? '✓' : '✗')).join(', ')}]`)

    // Send to all devices in background (if sessions exist)
    Promise.allSettled(
      devices.map(async (device) => {
        const { activeSession } = device
        if (!activeSession) {
          console.log(`[SendEvent] SKIP device=${device.deviceId.slice(0,8)} - no activeSession`)
          return
        }
        console.log(`[SendEvent] Sending to device=${device.deviceId.slice(0,8)} via session=${activeSession.name.slice(0,8)}`)
        const { event: verifiedEvent } = activeSession.sendEvent(event)
        await this.nostrPublish(verifiedEvent).catch(console.error)
      })
    )
      .then(() => {
        // Store recipient's user record
        this.storeUserRecord(recipientIdentityKey)
        // Also store owner's record if different (for sibling device sessions)
        // This ensures session state is persisted after ratcheting
        // TODO: check if really necessary, if yes, why?
        if (this.ownerPublicKey !== recipientIdentityKey) {
          this.storeUserRecord(this.ownerPublicKey)
        }
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
    const userRecord = this.userRecords.get(publicKey)
    const data: StoredUserRecord = {
      publicKey: publicKey,
      devices: Array.from(userRecord?.devices.entries() || []).map(
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
          hasResponderSession: device.hasResponderSession,
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

        for (const deviceData of data.devices) {
          const {
            deviceId,
            activeSession: serializedActive,
            inactiveSessions: serializedInactive,
            createdAt,
            staleAt,
            hasResponderSession,
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
              hasResponderSession,
            })
          } catch (e) {
            console.error(
              `Failed to deserialize session for user ${publicKey}, device ${deviceId}:`,
              e
            )
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
          } catch (e) {
            console.error("Migration error for user record:", e)
          }
        })
      )

      version = "1"
      await this.storage.put(this.versionKey(), version)
    }
  }
}
