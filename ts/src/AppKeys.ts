import { Filter, VerifiedEvent, UnsignedEvent, getPublicKey, verifyEvent } from "nostr-tools"
import * as nip44 from "nostr-tools/nip44"
import { applyAppKeysSnapshot } from "./multiDevice"
import { APP_KEYS_EVENT_KIND, NostrSubscribe, Unsubscribe } from "./types"

const now = () => Math.round(Date.now() / 1000)
const APP_KEYS_D_TAG = "double-ratchet/app-keys"

// Simplified tag format: ["device", identityPubkey, createdAt]
type DeviceTag = [
  type: "device",
  identityPubkey: string,
  createdAt: string,
]

const isDeviceTag = (tag: string[]): tag is DeviceTag =>
  tag.length >= 3 &&
  tag[0] === "device" &&
  typeof tag[1] === "string" &&
  typeof tag[2] === "string"

export function buildAppKeysFilter(authors?: string | string[]): Filter {
  const normalizedAuthors = Array.isArray(authors)
    ? authors.filter(Boolean)
    : authors ? [authors] : undefined

  // Some relays backfill stored parameterized replaceable events unreliably when
  // queried via #d, so fetch by author+kind and validate the d-tag client-side.
  return normalizedAuthors && normalizedAuthors.length > 0
    ? {
        kinds: [APP_KEYS_EVENT_KIND],
        authors: normalizedAuthors,
      }
    : {
        kinds: [APP_KEYS_EVENT_KIND],
      }
}

export function isAppKeysEvent(
  event: Pick<VerifiedEvent, "kind" | "tags">
): boolean {
  if (event.kind !== APP_KEYS_EVENT_KIND) {
    return false
  }

  return event.tags.some(
    (tag) => tag[0] === "d" && tag[1] === APP_KEYS_D_TAG
  )
}

/**
 * Device identity entry - contains only identity information.
 * identityPubkey serves as the device identifier.
 * Invite crypto material (ephemeral keys, shared secret) is in separate Invite events.
 */
export interface DeviceEntry {
  /** Identity public key - also serves as device identifier */
  identityPubkey: string
  createdAt: number
}

export interface DeviceLabels {
  deviceLabel?: string
  clientLabel?: string
  updatedAt: number
}

interface DeviceLabelsEntry extends DeviceLabels {
  identityPubkey: string
}

interface EncryptedAppKeysContent {
  type: "app-keys-labels"
  v: 1
  deviceLabels: DeviceLabelsEntry[]
}

type LegacyDeviceLabelsEntry = Partial<{
  identityPubkey: unknown
  identity_pubkey: unknown
  deviceLabel: unknown
  device_label: unknown
  clientLabel: unknown
  client_label: unknown
  updatedAt: unknown
  updated_at: unknown
}>

type LegacyEncryptedAppKeysContent = Partial<{
  type: unknown
  v: unknown
  deviceLabels: unknown
  device_labels: unknown
}>

const normalizeDeviceLabelsEntry = (value: unknown): DeviceLabelsEntry | null => {
  if (!value || typeof value !== "object") return null
  const entry = value as LegacyDeviceLabelsEntry
  const identityPubkey = entry.identityPubkey ?? entry.identity_pubkey
  const updatedAt = entry.updatedAt ?? entry.updated_at
  const deviceLabel = entry.deviceLabel ?? entry.device_label
  const clientLabel = entry.clientLabel ?? entry.client_label

  if (typeof identityPubkey !== "string" || typeof updatedAt !== "number") {
    return null
  }
  if (deviceLabel !== undefined && typeof deviceLabel !== "string") {
    return null
  }
  if (clientLabel !== undefined && typeof clientLabel !== "string") {
    return null
  }

  return {
    identityPubkey,
    updatedAt,
    ...(deviceLabel !== undefined ? { deviceLabel } : {}),
    ...(clientLabel !== undefined ? { clientLabel } : {}),
  }
}

/**
 * Manages a consolidated list of device invites (kind 30078, d-tag "double-ratchet/app-keys").
 * Single atomic event containing all device invites for a user.
 * Uses union merge strategy for conflict resolution.
 *
 * Note: ownerPublicKey is not stored - it's passed to getEvent() when publishing,
 * and NDK's signer sets the correct pubkey during signing anyway.
 */
export class AppKeys {
  private devices: Map<string, DeviceEntry> = new Map()
  private deviceLabels: Map<string, DeviceLabels> = new Map()

  constructor(devices: DeviceEntry[] = [], deviceLabels: DeviceLabelsEntry[] = []) {
    devices.forEach((device) => this.devices.set(device.identityPubkey, device))
    deviceLabels.forEach(({ identityPubkey, ...labels }) => {
      this.deviceLabels.set(identityPubkey, labels)
    })
  }

  /**
   * Creates a new device identity entry.
   * Note: This only creates the identity entry. The device must separately
   * create and publish its own Invite event with ephemeral keys.
   */
  createDeviceEntry(identityPubkey: string): DeviceEntry {
    return {
      identityPubkey,
      createdAt: now(),
    }
  }

  addDevice(device: DeviceEntry): void {
    if (!this.devices.has(device.identityPubkey)) {
      this.devices.set(device.identityPubkey, device)
    }
  }

  removeDevice(identityPubkey: string): void {
    this.devices.delete(identityPubkey)
    this.deviceLabels.delete(identityPubkey)
  }

  getDevice(identityPubkey: string): DeviceEntry | undefined {
    return this.devices.get(identityPubkey)
  }

  getAllDevices(): DeviceEntry[] {
    return Array.from(this.devices.values())
  }

  setDeviceLabels(
    identityPubkey: string,
    labels: {
      deviceLabel?: string
      clientLabel?: string
    },
    updatedAt = now()
  ): void {
    this.deviceLabels.set(identityPubkey, {
      deviceLabel: labels.deviceLabel,
      clientLabel: labels.clientLabel,
      updatedAt,
    })
  }

  getDeviceLabels(identityPubkey: string): DeviceLabels | undefined {
    return this.deviceLabels.get(identityPubkey)
  }

  getAllDeviceLabels(): DeviceLabelsEntry[] {
    return Array.from(this.deviceLabels.entries()).map(([identityPubkey, labels]) => ({
      identityPubkey,
      ...labels,
    }))
  }

  private getEncryptedContent(ownerPrivateKey?: Uint8Array): string {
    const deviceLabels = this.getAllDeviceLabels().filter(({ identityPubkey }) =>
      this.devices.has(identityPubkey)
    )

    if (deviceLabels.length === 0) {
      return ""
    }

    if (!ownerPrivateKey) {
      throw new Error("ownerPrivateKey is required to encrypt AppKeys labels")
    }

    const ownerPublicKey = getPublicKey(ownerPrivateKey)
    const conversationKey = nip44.v2.utils.getConversationKey(
      ownerPrivateKey,
      ownerPublicKey
    )
    const plaintext: EncryptedAppKeysContent = {
      type: "app-keys-labels",
      v: 1,
      deviceLabels,
    }

    return nip44.v2.encrypt(JSON.stringify(plaintext), conversationKey)
  }

  private loadEncryptedContent(content: string, ownerPrivateKey: Uint8Array): void {
    if (!content) return

    const ownerPublicKey = getPublicKey(ownerPrivateKey)
    const conversationKey = nip44.v2.utils.getConversationKey(
      ownerPrivateKey,
      ownerPublicKey
    )
    const decrypted = nip44.v2.decrypt(content, conversationKey)
    const payload = JSON.parse(decrypted) as LegacyEncryptedAppKeysContent

    if (payload.type !== "app-keys-labels" || payload.v !== 1) {
      throw new Error("Unsupported AppKeys label payload")
    }

    const rawLabelEntries = Array.isArray(payload.deviceLabels)
      ? payload.deviceLabels
      : Array.isArray(payload.device_labels) ? payload.device_labels : []
    const labelEntries = rawLabelEntries
      .map(normalizeDeviceLabelsEntry)
      .filter((entry): entry is DeviceLabelsEntry => entry !== null)

    this.deviceLabels.clear()
    labelEntries.forEach(({ identityPubkey, ...labels }) => {
      if (this.devices.has(identityPubkey)) {
        this.deviceLabels.set(identityPubkey, labels)
      }
    })
  }

  getEvent(ownerPrivateKey?: Uint8Array): UnsignedEvent {
    const deviceTags = this.getAllDevices().map((device) => [
      "device",
      device.identityPubkey,
      String(device.createdAt),
    ])

    return {
      kind: APP_KEYS_EVENT_KIND,
      pubkey: "", // Signer will set this
      content: this.getEncryptedContent(ownerPrivateKey),
      created_at: now(),
      tags: [
        ["d", APP_KEYS_D_TAG],
        ["version", "1"],
        ...deviceTags,
      ],
    }
  }

  static fromEvent(event: VerifiedEvent, ownerPrivateKey?: Uint8Array): AppKeys {
    if (!event.sig) {
      throw new Error("Event is not signed")
    }
    if (!verifyEvent(event)) {
      throw new Error("Event signature is invalid")
    }
    if (!isAppKeysEvent(event)) {
      throw new Error("Event is not an AppKeys snapshot")
    }

    // Simplified tag format: ["device", identityPubkey, createdAt]
    // Note: "removed" tags are ignored for backwards compatibility with old events
    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, identityPubkey, createdAt]) => ({
        identityPubkey,
        createdAt: parseInt(createdAt, 10) || event.created_at,
      }))

    const appKeys = new AppKeys(devices)
    if (ownerPrivateKey && event.content) {
      appKeys.loadEncryptedContent(event.content, ownerPrivateKey)
    }

    return appKeys
  }

  serialize(): string {
    return JSON.stringify({
      devices: this.getAllDevices(),
      deviceLabels: this.getAllDeviceLabels(),
    })
  }

  static deserialize(json: string): AppKeys {
    const data = JSON.parse(json) as {
      devices: DeviceEntry[]
      deviceLabels?: DeviceLabelsEntry[]
    }
    return new AppKeys(data.devices, data.deviceLabels || [])
  }

  merge(other: AppKeys): AppKeys {
    // Merge devices, preferring the one with earlier createdAt for same identityPubkey
    const mergedDevices = [...this.devices.values(), ...other.devices.values()]
      .reduce((map, device) => {
        const existing = map.get(device.identityPubkey)
        if (!existing || device.createdAt < existing.createdAt) {
          map.set(device.identityPubkey, device)
        }
        return map
      }, new Map<string, DeviceEntry>())

    const mergedLabels = [...this.deviceLabels.entries(), ...other.deviceLabels.entries()]
      .reduce((map, [identityPubkey, labels]) => {
        const existing = map.get(identityPubkey)
        if (!existing || labels.updatedAt > existing.updatedAt) {
          map.set(identityPubkey, labels)
        }
        return map
      }, new Map<string, DeviceLabels>())

    const mergedDeviceKeys = new Set(mergedDevices.keys())
    const deviceLabels = Array.from(mergedLabels.entries())
      .filter(([identityPubkey]) => mergedDeviceKeys.has(identityPubkey))
      .map(([identityPubkey, labels]) => ({
        identityPubkey,
        ...labels,
      }))

    return new AppKeys(Array.from(mergedDevices.values()), deviceLabels)
  }

  /**
   * Subscribe to AppKeys events from a user.
   * Similar to Invite.fromUser pattern.
   */
  static fromUser(
    user: string,
    subscribe: NostrSubscribe,
    onAppKeysList: (appKeys: AppKeys) => void
  ): Unsubscribe {
    return subscribe(
      buildAppKeysFilter(user),
      (event) => {
        if (event.pubkey !== user) return
        try {
          const appKeys = AppKeys.fromEvent(event)
          onAppKeysList(appKeys)
        } catch {
          // Invalid event
        }
      }
    )
  }

  /**
   * Wait for AppKeys from a user with timeout.
   * Returns the most recent AppKeys received within the timeout, or null.
   * Note: Uses the most recent event by created_at, not merging, since
   * device revocation is determined by absence from the list.
   */
  static waitFor(
    user: string,
    subscribe: NostrSubscribe,
    timeoutMs = 500
  ): Promise<AppKeys | null> {
    return new Promise((resolve) => {
      let latest: { list: AppKeys; createdAt: number } | null = null

      setTimeout(() => {
        unsubscribe()
        resolve(latest?.list ?? null)
      }, timeoutMs)

      const unsubscribe = subscribe(
        buildAppKeysFilter(user),
        (event) => {
          if (event.pubkey !== user) return
          try {
            const list = AppKeys.fromEvent(event)
            const next = applyAppKeysSnapshot({
              currentAppKeys: latest?.list,
              currentCreatedAt: latest?.createdAt,
              incomingAppKeys: list,
              incomingCreatedAt: event.created_at,
            })
            if (next.decision === "stale") {
              return
            }
            latest = { list: next.appKeys, createdAt: next.createdAt }
          } catch {
            // Invalid event
          }
        }
      )
    })
  }
}
