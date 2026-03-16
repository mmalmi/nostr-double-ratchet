import { Invite } from "../Invite"
import { Session } from "../Session"
import type {
  DeviceRecord as DeviceRecordShape,
  DeviceRecordDeps,
  DeviceSetupState,
  Unsubscribe,
} from "./types"

export class DeviceRecordActor implements DeviceRecordShape {
  private static readonly MAX_INACTIVE_SESSIONS = 10

  public activeSession?: Session
  public inactiveSessions: Session[] = []
  public state: DeviceSetupState = "new"
  public createdAt: number

  private ensurePromise?: Promise<void>
  private inviteSubscription?: Unsubscribe
  private sessionSubscriptions: Map<string, Unsubscribe> = new Map()
  private inviteAcceptancePromise?: Promise<Session>

  constructor(
    public readonly deviceId: string,
    private readonly deps: DeviceRecordDeps,
  ) {
    this.createdAt = deps.createdAt ?? Date.now()
  }

  private static sessionCanSend(session: Session): boolean {
    return Boolean(session.state.theirNextNostrPublicKey && session.state.ourCurrentNostrKey)
  }

  private static sessionCanReceive(session: Session): boolean {
    return Boolean(
      session.state.receivingChainKey ||
      session.state.theirCurrentNostrPublicKey ||
      session.state.receivingChainMessageNumber > 0
    )
  }

  private static sessionHasActivity(session: Session): boolean {
    return (
      session.state.sendingChainMessageNumber > 0 ||
      session.state.receivingChainMessageNumber > 0
    )
  }

  private static sessionPriority(session: Session): [number, number, number] {
    const canSend = DeviceRecordActor.sessionCanSend(session)
    const canReceive = DeviceRecordActor.sessionCanReceive(session)
    const directionality =
      canSend && canReceive ? 3
      : canSend ? 2
      : canReceive ? 1
      : 0

    return [
      directionality,
      session.state.receivingChainMessageNumber,
      session.state.sendingChainMessageNumber,
    ]
  }

  hasEstablishedActiveSession(): boolean {
    if (!this.activeSession) {
      return false
    }

    return (
      DeviceRecordActor.sessionCanSend(this.activeSession) &&
      (
        DeviceRecordActor.sessionCanReceive(this.activeSession) ||
        DeviceRecordActor.sessionHasActivity(this.activeSession)
      )
    )
  }

  ensureSetup(): Promise<void> {
    if (this.state === "revoked") {
      return Promise.resolve()
    }
    if (this.state === "session-ready") {
      return this.flushMessageQueue().catch(() => {})
    }
    if (this.ensurePromise) {
      return this.ensurePromise
    }

    this.ensurePromise = this.doEnsureSetup().finally(() => {
      this.ensurePromise = undefined
    })
    return this.ensurePromise
  }

  private async doEnsureSetup(): Promise<void> {
    if (this.state === "revoked") return

    if (this.activeSession) {
      this.state = "session-ready"
      await this.flushMessageQueue()
      return
    }

    if (this.deviceId === this.deps.ourDeviceId) {
      return
    }

    this.ensureInviteSubscription()
    this.state = "waiting-for-invite"
  }

  private ensureInviteSubscription(): void {
    if (this.inviteSubscription || this.state === "revoked") {
      return
    }

    this.inviteSubscription = Invite.fromUser(
      this.deviceId,
      this.deps.nostr.subscribe,
      (invite) => {
        this.acceptInvite(invite).catch(() => {})
      }
    )
  }

  acceptInvite(invite: Invite): Promise<Session> {
    if (this.state === "revoked") {
      return Promise.reject(new Error("Device is revoked"))
    }

    const inviteDeviceId = invite.deviceId || invite.inviter
    if (inviteDeviceId !== this.deviceId) {
      return Promise.reject(new Error("Invite does not target this device"))
    }

    if (this.hasEstablishedActiveSession()) {
      return Promise.resolve(this.activeSession!)
    }

    if (this.inviteAcceptancePromise) {
      return this.inviteAcceptancePromise
    }

    this.inviteAcceptancePromise = this.doAcceptInvite(invite).finally(() => {
      this.inviteAcceptancePromise = undefined
    })

    return this.inviteAcceptancePromise
  }

  private async doAcceptInvite(invite: Invite): Promise<Session> {
    this.state = "accepting-invite"

    try {
      const identityKey = this.deps.identityKey
      const encryptor = identityKey instanceof Uint8Array ? identityKey : identityKey.encrypt

      const { session, event } = await invite.accept(
        this.deps.nostr.subscribe,
        this.deps.ourDeviceId,
        encryptor,
        this.deps.ourOwnerPubkey
      )
      await this.deps.nostr.publish(event)
      this.installSession(session, false, { preferActive: true })
      this.state = "session-ready"
      await this.flushMessageQueue()
      return session
    } catch (error) {
      this.state = "waiting-for-invite"
      throw error
    }
  }

  installSession(
    session: Session,
    inactive = false,
    options: { persist?: boolean; preferActive?: boolean } = {}
  ): void {
    const { persist = true, preferActive = false } = options
    const promoteToActive = (nextSession: Session) => {
      const current = this.activeSession
      if (current === nextSession || current?.name === nextSession.name) {
        return
      }

      this.inactiveSessions = this.inactiveSessions.filter(
        (s) => s !== nextSession && s.name !== nextSession.name
      )

      const shouldReplaceUnestablishedActive =
        preferActive &&
        current &&
        !this.hasEstablishedActiveSession()

      if (shouldReplaceUnestablishedActive) {
        this.inactiveSessions.unshift(current)
        this.activeSession = nextSession
      } else if (
        current &&
        DeviceRecordActor.sessionPriority(current) >=
          DeviceRecordActor.sessionPriority(nextSession)
      ) {
        this.inactiveSessions.unshift(nextSession)
        this.activeSession = current
      } else {
        if (current) {
          this.inactiveSessions.unshift(current)
        }
        this.activeSession = nextSession
      }
      this.trimInactiveSessions()
    }

    if (inactive) {
      const exists = this.inactiveSessions.some(
        (s) => s === session || s.name === session.name
      )
      if (!exists) {
        this.inactiveSessions.unshift(session)
        this.trimInactiveSessions()
      }
    } else {
      promoteToActive(session)
    }

    if (!this.sessionSubscriptions.has(session.name)) {
      const unsub = session.onEvent((event) => {
        const owner = this.deps.ownerPubkey
        const isAuthorizedDevice =
          owner === this.deviceId ||
          this.deps.user.isDeviceAuthorized(this.deviceId) ||
          this.activeSession?.name === session.name
        if (!isAuthorizedDevice) {
          return
        }

        this.deps.user.onDeviceRumor(this.deviceId, event)
        promoteToActive(session)
        this.state = "session-ready"
        this.flushMessageQueue().catch(() => {})
        this.deps.user.onDeviceDirty()
      })
      this.sessionSubscriptions.set(session.name, unsub)
    }

    if (this.activeSession) {
      this.state = "session-ready"
    }

    if (persist) {
      this.deps.user.onDeviceDirty()
    }
  }

  private trimInactiveSessions(): void {
    if (this.inactiveSessions.length <= DeviceRecordActor.MAX_INACTIVE_SESSIONS) {
      return
    }
    const removed = this.inactiveSessions.splice(DeviceRecordActor.MAX_INACTIVE_SESSIONS)
    for (const session of removed) {
      const unsub = this.sessionSubscriptions.get(session.name)
      if (unsub) {
        unsub()
        this.sessionSubscriptions.delete(session.name)
      }
    }
  }

  async flushMessageQueue(): Promise<void> {
    if (!this.activeSession || this.state === "revoked") {
      return
    }

    const entries = await this.deps.messageQueue.getForTarget(this.deviceId)
    if (entries.length === 0) {
      return
    }

    for (const entry of entries) {
      try {
        const { event } = this.activeSession.sendEvent(entry.event)
        await this.deps.nostr.publish(event)
        await this.deps.messageQueue.removeByTargetAndEventId(this.deviceId, entry.event.id)
      } catch {
        // Keep entry for future retry.
      }
    }

    this.deps.user.onDeviceDirty()
  }

  deactivateCurrentSession(): void {
    if (!this.activeSession) return
    this.inactiveSessions.push(this.activeSession)
    this.activeSession = undefined
    this.state = "waiting-for-invite"
    this.deps.user.onDeviceDirty()
  }

  async revoke(): Promise<void> {
    this.state = "revoked"
    this.close()
    this.activeSession = undefined
    this.inactiveSessions = []
    await this.deps.messageQueue.removeForTarget(this.deviceId).catch(() => {})
    this.deps.user.onDeviceDirty()
  }

  close(): void {
    this.inviteSubscription?.()
    this.inviteSubscription = undefined

    for (const unsubscribe of this.sessionSubscriptions.values()) {
      unsubscribe()
    }
    this.sessionSubscriptions.clear()
  }
}
