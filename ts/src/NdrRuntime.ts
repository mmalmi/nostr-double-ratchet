import { AppKeys, buildAppKeysFilter, type DeviceEntry } from "./AppKeys"
import {
  AppKeysManager,
  DelegateManager,
  type DelegatePayload,
} from "./AppKeysManager"
import { Invite } from "./Invite"
import {
  applyAppKeysSnapshot,
  evaluateDeviceRegistrationState,
  shouldRequireRelayRegistrationConfirmation,
  type DeviceRegistrationState,
} from "./multiDevice"
import {
  SessionManager,
  type AcceptInviteOptions,
  type AcceptInviteResult,
  type OnEventCallback,
} from "./SessionManager"
import {
  InMemoryStorageAdapter,
  type StorageAdapter,
} from "./StorageAdapter"
import type { NostrPublish, NostrSubscribe, Unsubscribe } from "./types"

export interface NdrRuntimeOptions {
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
  sessionStorage?: StorageAdapter
  ownerIdentityKey?: Uint8Array
  appKeysFetchTimeoutMs?: number
  appKeysFastTimeoutMs?: number
}

export interface NdrRuntimeState extends DeviceRegistrationState {
  ownerPubkey: string | null
  currentDevicePubkey: string | null
  registeredDevices: DeviceEntry[]
  hasLocalAppKeys: boolean
  lastAppKeysCreatedAt: number
  appKeysManagerReady: boolean
  delegateManagerReady: boolean
  sessionManagerReady: boolean
  appKeysSubscriptionActive: boolean
}

export interface PrepareRegistrationOptions {
  ownerPubkey: string
  timeoutMs?: number
  deviceLabel?: string
  clientLabel?: string
}

export interface PrepareRegistrationForIdentityOptions
  extends PrepareRegistrationOptions {
  identityPubkey: string
}

export interface PreparedRegistration {
  appKeys: AppKeys
  devices: DeviceEntry[]
  baseDevices: DeviceEntry[]
  newDeviceIdentity: string
}

export interface PublishPreparedRegistrationResult {
  createdAt: number
  relayConfirmationRequired: boolean
}

export interface PrepareRevocationOptions {
  ownerPubkey: string
  identityPubkey: string
  timeoutMs?: number
}

export interface PreparedRevocation {
  appKeys: AppKeys
  devices: DeviceEntry[]
  revokedIdentity: string
}

export interface RegisterCurrentDeviceOptions
  extends PrepareRegistrationOptions {}

export interface RegisterDeviceIdentityOptions
  extends PrepareRegistrationForIdentityOptions {}

export interface RevokeDeviceOptions extends PrepareRevocationOptions {}

const DEFAULT_APP_KEYS_FETCH_TIMEOUT_MS = 10_000
const DEFAULT_APP_KEYS_FAST_TIMEOUT_MS = 2_000

const cloneAppKeys = (appKeys: AppKeys): AppKeys =>
  new AppKeys(appKeys.getAllDevices(), appKeys.getAllDeviceLabels())

const now = () => Math.floor(Date.now() / 1000)

export class NdrRuntime {
  private readonly nostrSubscribe: NostrSubscribe
  private readonly nostrPublish: NostrPublish
  private readonly storage: StorageAdapter
  private readonly sessionStorage: StorageAdapter
  private readonly ownerIdentityKey?: Uint8Array
  private readonly appKeysFetchTimeoutMs: number
  private readonly appKeysFastTimeoutMs: number

  private appKeysManager: AppKeysManager | null = null
  private delegateManager: DelegateManager | null = null
  private sessionManager: SessionManager | null = null

  private appKeysInitPromise: Promise<void> | null = null
  private delegateInitPromise: Promise<void> | null = null
  private sessionManagerInitPromise: Promise<SessionManager> | null = null

  private appKeysSubscriptionCleanup: Unsubscribe | null = null
  private appKeysSubscriptionOwnerPubkey: string | null = null

  private readonly stateListeners = new Set<(state: NdrRuntimeState) => void>()
  private readonly sessionEventCallbacks = new Set<OnEventCallback>()
  private readonly sessionEventCleanup = new Map<OnEventCallback, Unsubscribe>()

  private state: NdrRuntimeState = {
    ownerPubkey: null,
    currentDevicePubkey: null,
    registeredDevices: [],
    hasLocalAppKeys: false,
    lastAppKeysCreatedAt: 0,
    appKeysManagerReady: false,
    delegateManagerReady: false,
    sessionManagerReady: false,
    appKeysSubscriptionActive: false,
    isCurrentDeviceRegistered: false,
    hasKnownRegisteredDevices: false,
    noPreviousDevicesFound: true,
    requiresDeviceRegistration: false,
    canSendPrivateMessages: false,
  }

  constructor(options: NdrRuntimeOptions) {
    this.nostrSubscribe = options.nostrSubscribe
    this.nostrPublish = options.nostrPublish
    this.storage = options.storage || new InMemoryStorageAdapter()
    this.sessionStorage = options.sessionStorage || this.storage
    this.ownerIdentityKey = options.ownerIdentityKey
    this.appKeysFetchTimeoutMs =
      options.appKeysFetchTimeoutMs || DEFAULT_APP_KEYS_FETCH_TIMEOUT_MS
    this.appKeysFastTimeoutMs =
      options.appKeysFastTimeoutMs || DEFAULT_APP_KEYS_FAST_TIMEOUT_MS
  }

  getState(): NdrRuntimeState {
    return {
      ...this.state,
      registeredDevices: [...this.state.registeredDevices],
    }
  }

  onStateChange(listener: (state: NdrRuntimeState) => void): Unsubscribe {
    this.stateListeners.add(listener)
    listener(this.getState())
    return () => {
      this.stateListeners.delete(listener)
    }
  }

  onSessionEvent(callback: OnEventCallback): Unsubscribe {
    this.sessionEventCallbacks.add(callback)
    if (this.sessionManager && !this.sessionEventCleanup.has(callback)) {
      this.sessionEventCleanup.set(callback, this.sessionManager.onEvent(callback))
    }
    return () => {
      this.sessionEventCallbacks.delete(callback)
      const cleanup = this.sessionEventCleanup.get(callback)
      cleanup?.()
      this.sessionEventCleanup.delete(callback)
    }
  }

  getAppKeysManager(): AppKeysManager | null {
    return this.appKeysManager
  }

  getDelegateManager(): DelegateManager | null {
    return this.delegateManager
  }

  getSessionManager(): SessionManager | null {
    return this.sessionManager
  }

  async initManagers(): Promise<void> {
    await Promise.all([this.initAppKeysManager(), this.initDelegateManager()])
  }

  async initForOwner(ownerPubkey: string): Promise<SessionManager> {
    await this.initManagers()
    const manager = await this.initSessionManager(ownerPubkey)
    this.startAppKeysSubscription(ownerPubkey)
    return manager
  }

  async waitForSessionManager(ownerPubkey?: string): Promise<SessionManager> {
    if (this.sessionManager) {
      return this.sessionManager
    }

    if (!ownerPubkey) {
      throw new Error("Owner pubkey required to initialize SessionManager")
    }

    return this.initForOwner(ownerPubkey)
  }

  async initAppKeysManager(): Promise<void> {
    if (this.appKeysManager) return
    if (this.appKeysInitPromise) return this.appKeysInitPromise

    this.appKeysInitPromise = (async () => {
      const manager = new AppKeysManager({
        nostrPublish: this.nostrPublish,
        storage: this.storage,
        ownerIdentityKey: this.ownerIdentityKey,
      })
      await manager.init()
      this.appKeysManager = manager
      const appKeys = manager.getAppKeys()
      this.syncState({
        appKeysManagerReady: true,
        registeredDevices: manager.getOwnDevices(),
        hasLocalAppKeys: !!(appKeys && appKeys.getAllDevices().length > 0),
      })
    })().finally(() => {
      this.appKeysInitPromise = null
    })

    return this.appKeysInitPromise
  }

  async initDelegateManager(): Promise<void> {
    if (this.delegateManager) return
    if (this.delegateInitPromise) return this.delegateInitPromise

    this.delegateInitPromise = (async () => {
      const manager = new DelegateManager({
        nostrSubscribe: this.nostrSubscribe,
        nostrPublish: this.nostrPublish,
        storage: this.storage,
      })
      await manager.init()
      this.delegateManager = manager
      this.syncState({
        delegateManagerReady: true,
        currentDevicePubkey: manager.getIdentityPublicKey(),
        ownerPubkey: manager.getOwnerPublicKey(),
      })
    })().finally(() => {
      this.delegateInitPromise = null
    })

    return this.delegateInitPromise
  }

  async initSessionManager(ownerPubkey: string): Promise<SessionManager> {
    if (this.sessionManager) {
      if (this.state.ownerPubkey && this.state.ownerPubkey !== ownerPubkey) {
        throw new Error(
          `NdrRuntime already initialized for owner ${this.state.ownerPubkey}`
        )
      }
      return this.sessionManager
    }
    if (this.sessionManagerInitPromise) {
      return this.sessionManagerInitPromise
    }

    this.sessionManagerInitPromise = (async () => {
      await this.initDelegateManager()
      if (!this.delegateManager) {
        throw new Error("DelegateManager not initialized")
      }

      await this.delegateManager.activate(ownerPubkey)
      const manager = this.delegateManager.createSessionManager(this.sessionStorage)
      this.attachSessionEventCallbacks(manager)
      await manager.init()
      this.sessionManager = manager
      this.syncState({
        ownerPubkey,
        sessionManagerReady: true,
      })
      return manager
    })().catch((error) => {
      this.clearAttachedSessionEventCallbacks()
      throw error
    }).finally(() => {
      this.sessionManagerInitPromise = null
    })

    return this.sessionManagerInitPromise
  }

  async resolveBaseAppKeys(
    ownerPubkey: string,
    timeoutMs: number = this.appKeysFetchTimeoutMs
  ): Promise<AppKeys> {
    const initialTimeoutMs = Math.min(this.appKeysFastTimeoutMs, timeoutMs)
    try {
      const existingKeys = await AppKeys.waitFor(
        ownerPubkey,
        this.nostrSubscribe,
        initialTimeoutMs
      )
      if (existingKeys) {
        return existingKeys
      }
    } catch {
      // Ignore relay fetch failures and fall back to local state.
    }

    const localKeys = this.appKeysManager?.getAppKeys()
    if (localKeys && localKeys.getAllDevices().length > 0) {
      return cloneAppKeys(localKeys)
    }

    if (timeoutMs > initialTimeoutMs) {
      try {
        const remaining = Math.max(timeoutMs - initialTimeoutMs, 0)
        const existingKeys = await AppKeys.waitFor(
          ownerPubkey,
          this.nostrSubscribe,
          remaining
        )
        if (existingKeys) {
          return existingKeys
        }
      } catch {
        // Ignore relay fetch failures.
      }
    }

    return new AppKeys()
  }

  startAppKeysSubscription(ownerPubkey: string): void {
    if (
      this.appKeysSubscriptionCleanup &&
      this.appKeysSubscriptionOwnerPubkey === ownerPubkey
    ) {
      return
    }

    this.stopAppKeysSubscription()
    this.appKeysSubscriptionOwnerPubkey = ownerPubkey

    this.appKeysSubscriptionCleanup = this.nostrSubscribe(
      buildAppKeysFilter(ownerPubkey),
      async (event) => {
        if (event.pubkey !== ownerPubkey) return
        try {
          const incomingAppKeys = AppKeys.fromEvent(event)
          await this.applyIncomingAppKeys(incomingAppKeys, event.created_at)
        } catch {
          // Ignore invalid AppKeys events.
        }
      }
    )

    this.syncState({
      ownerPubkey,
      appKeysSubscriptionActive: true,
    })
  }

  stopAppKeysSubscription(): void {
    this.appKeysSubscriptionCleanup?.()
    this.appKeysSubscriptionCleanup = null
    this.appKeysSubscriptionOwnerPubkey = null
    this.syncState({
      appKeysSubscriptionActive: false,
    })
  }

  async refreshOwnAppKeysFromRelay(
    ownerPubkey: string,
    timeoutMs: number = this.appKeysFastTimeoutMs
  ): Promise<boolean> {
    const nextSnapshot = await AppKeys.waitFor(
      ownerPubkey,
      this.nostrSubscribe,
      timeoutMs
    )
    if (!nextSnapshot) {
      return false
    }

    const update = await this.applyIncomingAppKeys(nextSnapshot, now())
    return update !== "stale"
  }

  async prepareRegistration(
    options: PrepareRegistrationOptions
  ): Promise<PreparedRegistration> {
    await this.initManagers()
    if (!this.delegateManager) {
      throw new Error("DelegateManager not initialized")
    }

    const baseKeys = await this.resolveBaseAppKeys(
      options.ownerPubkey,
      options.timeoutMs
    )
    const appKeys = cloneAppKeys(baseKeys)

    const payload = this.buildRegistrationPayload(this.delegateManager, options)
    appKeys.addDevice({
      identityPubkey: payload.identityPubkey,
      createdAt: now(),
    })
    if (payload.deviceLabel || payload.clientLabel) {
      appKeys.setDeviceLabels(payload.identityPubkey, payload)
    }

    return {
      appKeys,
      devices: appKeys.getAllDevices(),
      baseDevices: baseKeys.getAllDevices(),
      newDeviceIdentity: payload.identityPubkey,
    }
  }

  async prepareRegistrationForIdentity(
    options: PrepareRegistrationForIdentityOptions
  ): Promise<PreparedRegistration> {
    await this.initAppKeysManager()

    const baseKeys = await this.resolveBaseAppKeys(
      options.ownerPubkey,
      options.timeoutMs
    )
    const appKeys = cloneAppKeys(baseKeys)
    appKeys.addDevice({
      identityPubkey: options.identityPubkey,
      createdAt: now(),
    })
    if (options.deviceLabel || options.clientLabel) {
      appKeys.setDeviceLabels(options.identityPubkey, options)
    }

    return {
      appKeys,
      devices: appKeys.getAllDevices(),
      baseDevices: baseKeys.getAllDevices(),
      newDeviceIdentity: options.identityPubkey,
    }
  }

  async publishPreparedRegistration(
    prepared: PreparedRegistration
  ): Promise<PublishPreparedRegistrationResult> {
    await this.initAppKeysManager()
    const relayConfirmationRequired = shouldRequireRelayRegistrationConfirmation({
      currentDevicePubkey: this.state.currentDevicePubkey,
      registeredDevices: prepared.baseDevices,
      hasLocalAppKeys: prepared.baseDevices.length > 0,
      appKeysManagerReady: this.state.appKeysManagerReady,
      sessionManagerReady: this.state.sessionManagerReady,
    })
    const publishedEvent = await this.publishAppKeys(prepared.appKeys)
    await this.appKeysManager?.setAppKeys(prepared.appKeys)
    this.syncState({
      registeredDevices: prepared.devices,
      hasLocalAppKeys: prepared.devices.length > 0,
      lastAppKeysCreatedAt: publishedEvent.created_at ?? now(),
    })
    return {
      createdAt: publishedEvent.created_at ?? now(),
      relayConfirmationRequired,
    }
  }

  async prepareRevocation(
    options: PrepareRevocationOptions
  ): Promise<PreparedRevocation> {
    const baseKeys = await this.resolveBaseAppKeys(
      options.ownerPubkey,
      options.timeoutMs
    )
    const appKeys = cloneAppKeys(baseKeys)
    appKeys.removeDevice(options.identityPubkey)
    return {
      appKeys,
      devices: appKeys.getAllDevices(),
      revokedIdentity: options.identityPubkey,
    }
  }

  async publishPreparedRevocation(
    prepared: PreparedRevocation
  ): Promise<number> {
    await this.initAppKeysManager()
    const publishedEvent = await this.publishAppKeys(prepared.appKeys)
    await this.appKeysManager?.setAppKeys(prepared.appKeys)
    this.syncState({
      registeredDevices: prepared.devices,
      hasLocalAppKeys: prepared.devices.length > 0,
      lastAppKeysCreatedAt: publishedEvent.created_at ?? now(),
    })
    return publishedEvent.created_at ?? now()
  }

  async registerCurrentDevice(
    options: RegisterCurrentDeviceOptions
  ): Promise<PublishPreparedRegistrationResult> {
    const prepared = await this.prepareRegistration(options)
    const result = await this.publishPreparedRegistration(prepared)
    if (result.relayConfirmationRequired) {
      await this.waitForCurrentDeviceRegistrationOnRelay(
        options.ownerPubkey,
        prepared.newDeviceIdentity,
        options.timeoutMs || this.appKeysFetchTimeoutMs
      )
      await this.refreshOwnAppKeysFromRelay(
        options.ownerPubkey,
        options.timeoutMs || this.appKeysFastTimeoutMs
      ).catch(() => {})
    }
    return {
      createdAt: result.createdAt,
      relayConfirmationRequired: result.relayConfirmationRequired,
    }
  }

  async registerDeviceIdentity(
    options: RegisterDeviceIdentityOptions
  ): Promise<PublishPreparedRegistrationResult> {
    const prepared = await this.prepareRegistrationForIdentity(options)
    return this.publishPreparedRegistration(prepared)
  }

  async revokeDevice(options: RevokeDeviceOptions): Promise<number> {
    const prepared = await this.prepareRevocation(options)
    return this.publishPreparedRevocation(prepared)
  }

  async ensureCurrentDeviceRegistered(
    ownerPubkey: string,
    timeoutMs?: number
  ): Promise<boolean> {
    await this.initManagers()
    if (this.state.isCurrentDeviceRegistered) {
      return false
    }

    await this.registerCurrentDevice({
      ownerPubkey,
      timeoutMs,
    })
    return true
  }

  async republishInvite(): Promise<void> {
    await this.initDelegateManager()
    if (!this.delegateManager) {
      throw new Error("DelegateManager not initialized")
    }
    await this.delegateManager.publishInvite()
  }

  async rotateInvite(): Promise<void> {
    await this.initDelegateManager()
    if (!this.delegateManager) {
      throw new Error("DelegateManager not initialized")
    }
    await this.delegateManager.rotateInvite()
  }

  async createLinkInvite(ownerPubkey?: string): Promise<Invite> {
    await this.initDelegateManager()
    if (!this.delegateManager) {
      throw new Error("DelegateManager not initialized")
    }
    const baseInvite = this.delegateManager.getInvite()
    if (!baseInvite) {
      throw new Error("DelegateManager invite not initialized")
    }
    const invite = Invite.deserialize(baseInvite.serialize())
    invite.purpose = "link"
    if (ownerPubkey) {
      invite.ownerPubkey = ownerPubkey
    }
    return invite
  }

  async acceptInvite(
    invite: Invite,
    options?: AcceptInviteOptions
  ): Promise<AcceptInviteResult> {
    const ownerPubkey =
      options?.ownerPublicKey || this.state.ownerPubkey || invite.ownerPubkey || invite.inviter
    const manager = await this.waitForSessionManager(ownerPubkey)
    return manager.acceptInvite(invite, options)
  }

  async acceptLinkInvite(
    invite: Invite,
    ownerPubkey: string
  ): Promise<AcceptInviteResult> {
    return this.acceptInvite(invite, {
      ownerPublicKey: ownerPubkey,
    })
  }

  close(): void {
    this.stopAppKeysSubscription()
    this.clearAttachedSessionEventCallbacks()
    this.appKeysManager?.close()
    this.delegateManager?.close()
    this.sessionManager?.close()
    this.appKeysManager = null
    this.delegateManager = null
    this.sessionManager = null
    this.appKeysInitPromise = null
    this.delegateInitPromise = null
    this.sessionManagerInitPromise = null
    this.syncState({
      ownerPubkey: null,
      currentDevicePubkey: null,
      registeredDevices: [],
      hasLocalAppKeys: false,
      lastAppKeysCreatedAt: 0,
      appKeysManagerReady: false,
      delegateManagerReady: false,
      sessionManagerReady: false,
      appKeysSubscriptionActive: false,
    })
  }

  private attachSessionEventCallbacks(manager: SessionManager): void {
    this.clearAttachedSessionEventCallbacks()
    for (const callback of this.sessionEventCallbacks) {
      this.sessionEventCleanup.set(callback, manager.onEvent(callback))
    }
  }

  private clearAttachedSessionEventCallbacks(): void {
    for (const cleanup of this.sessionEventCleanup.values()) {
      cleanup()
    }
    this.sessionEventCleanup.clear()
  }

  private buildRegistrationPayload(
    delegateManager: DelegateManager,
    options: Pick<PrepareRegistrationOptions, "deviceLabel" | "clientLabel">
  ): DelegatePayload {
    const payload = delegateManager.getRegistrationPayload()
    return {
      ...payload,
      ...(options.deviceLabel ? { deviceLabel: options.deviceLabel } : {}),
      ...(options.clientLabel ? { clientLabel: options.clientLabel } : {}),
    }
  }

  private async applyIncomingAppKeys(
    incomingAppKeys: AppKeys,
    incomingCreatedAt: number
  ): Promise<"advanced" | "stale" | "merged_equal_timestamp"> {
    await this.initAppKeysManager()
    const update = applyAppKeysSnapshot({
      currentAppKeys: this.appKeysManager?.getAppKeys(),
      currentCreatedAt: this.state.lastAppKeysCreatedAt,
      incomingAppKeys,
      incomingCreatedAt,
    })
    if (update.decision === "stale") {
      return update.decision
    }

    await this.appKeysManager?.setAppKeys(update.appKeys)
    this.syncState({
      registeredDevices: update.appKeys.getAllDevices(),
      hasLocalAppKeys: update.appKeys.getAllDevices().length > 0,
      lastAppKeysCreatedAt: update.createdAt,
    })
    return update.decision
  }

  private async publishAppKeys(appKeys: AppKeys) {
    return this.nostrPublish(appKeys.getEvent(this.ownerIdentityKey))
  }

  private async waitForCurrentDeviceRegistrationOnRelay(
    ownerPubkey: string,
    devicePubkey: string,
    timeoutMs: number
  ): Promise<void> {
    const appKeys = await AppKeys.waitFor(ownerPubkey, this.nostrSubscribe, timeoutMs)
    const isAuthorized =
      appKeys?.getAllDevices().some((device) => device.identityPubkey === devicePubkey) ??
      false

    if (!isAuthorized) {
      throw new Error(
        `Relay AppKeys for ${ownerPubkey} do not include current device ${devicePubkey}`
      )
    }
  }

  private syncState(
    patch: Partial<
      Omit<
        NdrRuntimeState,
        | "isCurrentDeviceRegistered"
        | "hasKnownRegisteredDevices"
        | "noPreviousDevicesFound"
        | "requiresDeviceRegistration"
        | "canSendPrivateMessages"
      >
    >
  ): void {
    const nextState = {
      ...this.state,
      ...patch,
    }
    const derived = evaluateDeviceRegistrationState({
      currentDevicePubkey: nextState.currentDevicePubkey,
      registeredDevices: nextState.registeredDevices,
      hasLocalAppKeys: nextState.hasLocalAppKeys,
      appKeysManagerReady: nextState.appKeysManagerReady,
      sessionManagerReady: nextState.sessionManagerReady,
    })
    this.state = {
      ...nextState,
      ...derived,
    }
    for (const listener of this.stateListeners) {
      listener(this.getState())
    }
  }
}
