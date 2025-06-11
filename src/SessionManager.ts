import { CHAT_MESSAGE_KIND, NostrPublish, NostrSubscribe, Rumor, Unsubscribe } from "./types"
import { UserRecord } from "./UserRecord"
import { Invite } from "./Invite"
import { getPublicKey } from "nostr-tools"

export default class SessionManager {
    private userRecords: Map<string, UserRecord> = new Map()
    private nostrSubscribe: NostrSubscribe
    private nostrPublish: NostrPublish
    private ourIdentityKey: Uint8Array
    private inviteUnsubscribes: Map<string, Unsubscribe> = new Map()
    private deviceId: string
    private invite: Invite

    constructor(ourIdentityKey: Uint8Array, deviceId: string, nostrSubscribe: NostrSubscribe, nostrPublish: NostrPublish) {
        this.userRecords = new Map()
        this.nostrSubscribe = nostrSubscribe
        this.nostrPublish = nostrPublish
        this.ourIdentityKey = ourIdentityKey
        this.deviceId = deviceId

        // Create our invite
        const ourPublicKey = getPublicKey(this.ourIdentityKey)
        this.invite = Invite.createNew(ourPublicKey, this.deviceId)

        // Publish invite to Nostr
        const inviteEvent = this.invite.getEvent()
        this.nostrPublish(inviteEvent)

        // Subscribe to our own invites
        Invite.fromUser(ourPublicKey, this.nostrSubscribe, async (invite) => {
            try {
                // Extract device name from invite event tags
                const inviteDeviceId = invite.getEvent().tags.find(tag => tag[0] === 'd')?.[1]?.split('/')?.[2]
                if (!inviteDeviceId || inviteDeviceId === this.deviceId) {
                    return // Ignore invites without device name or from our own device
                }

                // Check if we already have a session with this device
                const existingRecord = this.userRecords.get(ourPublicKey)
                if (existingRecord?.getActiveSessions().some(session => session.name === inviteDeviceId)) {
                    return // Ignore invites from devices we already have sessions with
                }

                const { session, event } = await invite.accept(
                    this.nostrSubscribe,
                    ourPublicKey,
                    this.ourIdentityKey
                )
                this.nostrPublish(event)

                // Store the new session
                let userRecord = this.userRecords.get(ourPublicKey)
                if (!userRecord) {
                    userRecord = new UserRecord(ourPublicKey, this.nostrSubscribe)
                    this.userRecords.set(ourPublicKey, userRecord)
                }
                userRecord.insertSession(inviteDeviceId, session)

                // Set up event handling for the new session
                session.onEvent((_event) => {
                    this.internalSubscriptions.forEach(callback => callback(_event))
                })
            } catch {
                // Ignore failed invites
            }
        })
    }

    getDeviceId(): string {
        return this.deviceId
    }

    getInvite(): Invite {
        return this.invite
    }

    async sendText(recipientIdentityKey: string, text: string) {
        const event = {
            kind: CHAT_MESSAGE_KIND,
            content: text,
        }
        return await this.sendEvent(recipientIdentityKey, event)
    }

    async sendEvent(recipientIdentityKey: string, event: Partial<Rumor>) {
        const results = []
        
        // Send to recipient's devices
        const userRecord = this.userRecords.get(recipientIdentityKey)
        if (!userRecord) {
            // Listen for invites from recipient
            this.listenToUser(recipientIdentityKey)
            throw new Error("No active session with user. Listening for invites.")
        }

        // Send to all active sessions with recipient
        for (const session of userRecord.getActiveSessions()) {
            const { event: encryptedEvent } = session.sendEvent(event)
            results.push(encryptedEvent)
        }

        // Send to our own devices (for multi-device sync)
        const ourPublicKey = getPublicKey(this.ourIdentityKey)
        const ownUserRecord = this.userRecords.get(ourPublicKey)
        if (ownUserRecord) {
            for (const session of ownUserRecord.getActiveSessions()) {
                const { event: encryptedEvent } = session.sendEvent(event)
                results.push(encryptedEvent)
            }
        }

        return results
    }

    listenToUser(userPubkey: string) {
        // Don't subscribe multiple times to the same user
        if (this.inviteUnsubscribes.has(userPubkey)) return

        const unsubscribe = Invite.fromUser(userPubkey, this.nostrSubscribe, async (_invite) => {
            try {
                const { session, event } = await _invite.accept(
                    this.nostrSubscribe,
                    getPublicKey(this.ourIdentityKey),
                    this.ourIdentityKey
                )
                this.nostrPublish(event)

                // Store the new session
                let userRecord = this.userRecords.get(userPubkey)
                if (!userRecord) {
                    userRecord = new UserRecord(userPubkey, this.nostrSubscribe)
                    this.userRecords.set(userPubkey, userRecord)
                }
                userRecord.insertSession(event.id || 'unknown', session)

                // Set up event handling for the new session
                session.onEvent((_event) => {
                    this.internalSubscriptions.forEach(callback => callback(_event))
                })

                // Return the event to be published
                return event
            } catch {
            }
        })

        this.inviteUnsubscribes.set(userPubkey, unsubscribe)
    }

    stopListeningToUser(userPubkey: string) {
        const unsubscribe = this.inviteUnsubscribes.get(userPubkey)
        if (unsubscribe) {
            unsubscribe()
            this.inviteUnsubscribes.delete(userPubkey)
        }
    }

    // Update onEvent to include internalSubscriptions management
    private internalSubscriptions: Set<(_event: Rumor) => void> = new Set()

    onEvent(callback: (_event: Rumor) => void) {
        this.internalSubscriptions.add(callback)

        // Subscribe to existing sessions
        for (const userRecord of this.userRecords.values()) {
            for (const session of userRecord.getActiveSessions()) {
                session.onEvent((_event: Rumor) => {
                    callback(_event)
                })
            }
        }

        // Return unsubscribe function
        return () => {
            this.internalSubscriptions.delete(callback)
        }
    }

    close() {
        // Clean up all subscriptions
        for (const unsubscribe of this.inviteUnsubscribes.values()) {
            unsubscribe()
        }
        this.inviteUnsubscribes.clear()
        
        // Close all sessions
        for (const userRecord of this.userRecords.values()) {
            for (const session of userRecord.getActiveSessions()) {
                session.close()
            }
        }
        this.userRecords.clear()
        this.internalSubscriptions.clear()
    }
}
