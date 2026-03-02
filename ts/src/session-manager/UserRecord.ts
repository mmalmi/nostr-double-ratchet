import { AppKeys } from "../AppKeys"
import { APP_KEYS_EVENT_KIND } from "../types"
import type { Rumor } from "../types"
import { DeviceRecord } from "./DeviceRecord"
import type {
  UserRecord as UserRecordShape,
  UserRecordDeps,
  UserSetupState,
  Unsubscribe,
} from "./types"

export class UserRecord implements UserRecordShape {
  public appKeys?: AppKeys
  public state: UserSetupState = "new"
  public devices: Map<string, DeviceRecord> = new Map()
  public setupPromise?: Promise<void>

  private appKeysSubscription?: Unsubscribe
  private latestAppKeysCreatedAt = 0

  constructor(
    public readonly publicKey: string,
    private readonly deps: UserRecordDeps,
  ) {}

  private setState(nextState: UserSetupState): void {
    if (this.state === nextState) {
      return
    }
    this.state = nextState
    this.deps.onSetupStateChange(this.publicKey)
  }

  ensureDevice(deviceId: string, createdAt?: number): DeviceRecord {
    if (!deviceId) {
      throw new Error("Device record must include a deviceId")
    }
    const existing = this.devices.get(deviceId)
    if (existing) {
      return existing
    }
    const device = new DeviceRecord(deviceId, {
      ownerPubkey: this.publicKey,
      user: this,
      nostr: this.deps.nostr,
      messageQueue: this.deps.messageQueue,
      ourDeviceId: this.deps.ourDeviceId,
      ourOwnerPubkey: this.deps.ourOwnerPubkey,
      identityKey: this.deps.identityKey,
      createdAt,
    })
    this.devices.set(deviceId, device)
    return device
  }

  setAppKeys(appKeys: AppKeys | undefined): void {
    this.appKeys = appKeys
  }

  async queueOutboundMessage(rumor: Rumor): Promise<void> {
    if (!this.appKeys) {
      await this.deps.discoveryQueue.add(this.publicKey, rumor)
      return
    }

    const deviceIds = this.getTargetDeviceIds()
    if (deviceIds.length === 0) {
      await this.deps.discoveryQueue.add(this.publicKey, rumor)
      return
    }

    for (const deviceId of deviceIds) {
      await this.deps.messageQueue.add(deviceId, rumor)
    }
  }

  ensureSetup(): Promise<void> {
    if (this.state === "ready") {
      for (const deviceId of this.getTargetDeviceIds()) {
        this.ensureDevice(deviceId).ensureSetup().catch(() => {})
      }
      return Promise.resolve()
    }
    if (this.setupPromise) {
      return this.setupPromise
    }
    this.setupPromise = this.doEnsureSetup().finally(() => {
      this.setupPromise = undefined
    })
    return this.setupPromise
  }

  private async doEnsureSetup(): Promise<void> {
    this.ensureAppKeysSubscription()

    if (!this.appKeys) {
      this.setState("fetching-appkeys")
      const appKeys = await this.fetchAppKeys().catch(() => null)
      if (appKeys) {
        await this.onAppKeys(appKeys)
      }
    }

    if (!this.appKeys) {
      this.setState("stale")
      return
    }

    this.setState("appkeys-known")
    await this.expandDiscoveryQueue()

    for (const deviceId of this.getTargetDeviceIds()) {
      const device = this.ensureDevice(deviceId)
      device.ensureSetup().catch(() => {})
    }

    this.setState("ready")
  }

  async onAppKeys(appKeys: AppKeys): Promise<void> {
    this.appKeys = appKeys
    this.setState("appkeys-known")
    this.deps.manager.updateDelegateMapping(this.publicKey, appKeys)

    const activeIds = new Set(
      appKeys.getAllDevices()
        .map((d) => d.identityPubkey)
        .filter(Boolean) as string[]
    )

    for (const [deviceId, device] of this.devices) {
      if (!activeIds.has(deviceId)) {
        await device.revoke()
        this.devices.delete(deviceId)
        this.deps.manager.removeDelegateMapping(deviceId)
      }
    }

    for (const deviceId of activeIds) {
      this.ensureDevice(deviceId)
    }

    await this.expandDiscoveryQueue()

    for (const deviceId of this.getTargetDeviceIds()) {
      this.devices.get(deviceId)?.ensureSetup().catch(() => {})
    }

    this.setState("ready")
    this.onDeviceDirty()
  }

  private ensureAppKeysSubscription(): void {
    if (this.appKeysSubscription) {
      return
    }

    this.appKeysSubscription = this.deps.nostr.subscribe(
      {
        kinds: [APP_KEYS_EVENT_KIND],
        authors: [this.publicKey],
        "#d": ["double-ratchet/app-keys"],
      },
      (event) => {
        if (event.created_at < this.latestAppKeysCreatedAt) return
        this.latestAppKeysCreatedAt = event.created_at
        try {
          const appKeys = AppKeys.fromEvent(event)
          this.onAppKeys(appKeys).catch(() => {})
        } catch {
          // Ignore invalid AppKeys events.
        }
      }
    )
  }

  private fetchAppKeys(timeoutMs = 2000): Promise<AppKeys | null> {
    return new Promise((resolve) => {
      let latestEvent: { created_at: number; appKeys: AppKeys } | null = null
      let resolved = false

      const resolveResult = () => {
        if (resolved) return
        resolved = true
        unsubscribe()
        resolve(latestEvent?.appKeys ?? null)
      }

      const timeout = setTimeout(resolveResult, timeoutMs)
      const unsubscribe = this.deps.nostr.subscribe(
        {
          kinds: [APP_KEYS_EVENT_KIND],
          authors: [this.publicKey],
          "#d": ["double-ratchet/app-keys"],
        },
        (event) => {
          if (resolved) return
          try {
            const appKeys = AppKeys.fromEvent(event)
            if (!latestEvent || event.created_at >= latestEvent.created_at) {
              latestEvent = { created_at: event.created_at, appKeys }
            }
            clearTimeout(timeout)
            setTimeout(resolveResult, 100)
          } catch {
            // Ignore invalid AppKeys events.
          }
        }
      )
    })
  }

  private async expandDiscoveryQueue(): Promise<void> {
    const entries = await this.deps.discoveryQueue.getForTarget(this.publicKey)
    if (entries.length === 0) {
      return
    }

    const deviceIds = this.getTargetDeviceIds()
    if (deviceIds.length === 0) {
      return
    }

    for (const entry of entries) {
      try {
        for (const deviceId of deviceIds) {
          await this.deps.messageQueue.add(deviceId, entry.event)
        }
      } finally {
        await this.deps.discoveryQueue.remove(entry.id).catch(() => {})
      }
    }
  }

  private getTargetDeviceIds(): string[] {
    if (!this.appKeys) {
      return []
    }
    return this.appKeys.getAllDevices()
      .map((d) => d.identityPubkey)
      .filter((deviceId): deviceId is string =>
        Boolean(deviceId) && deviceId !== this.deps.ourDeviceId
      )
  }

  isDeviceAuthorized(deviceId: string): boolean {
    if (this.publicKey === deviceId) {
      return true
    }
    if (!this.appKeys) return false
    return this.appKeys.getAllDevices().some((d) => d.identityPubkey === deviceId)
  }

  onDeviceRumor(deviceId: string, rumor: Rumor): void {
    this.deps.manager.handleDeviceRumor(this.publicKey, deviceId, rumor)
  }

  onDeviceDirty(): void {
    this.deps.manager.persistUserRecord(this.publicKey)
  }

  deactivateCurrentSessions(): void {
    for (const device of this.devices.values()) {
      device.deactivateCurrentSession()
    }
  }

  close(): void {
    this.appKeysSubscription?.()
    this.appKeysSubscription = undefined
    for (const device of this.devices.values()) {
      device.close()
    }
  }
}
