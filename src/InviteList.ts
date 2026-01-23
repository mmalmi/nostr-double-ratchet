import { VerifiedEvent, UnsignedEvent, verifyEvent } from "nostr-tools"
import { INVITE_LIST_EVENT_KIND } from "./types"

const now = () => Math.round(Date.now() / 1000)

// Simplified tag format: ["device", identityPubkey, createdAt]
type DeviceTag = [
  type: "device",
  identityPubkey: string,
  createdAt: string,
]

// Simplified removed tag format: ["removed", identityPubkey, removedAt]
type RemovedTag = [type: "removed", identityPubkey: string, removedAt: string]

const isDeviceTag = (tag: string[]): tag is DeviceTag =>
  tag.length >= 3 &&
  tag[0] === "device" &&
  typeof tag[1] === "string" &&
  typeof tag[2] === "string"

const isRemovedTag = (tag: string[]): tag is RemovedTag =>
  tag.length >= 3 &&
  tag[0] === "removed" &&
  typeof tag[1] === "string" &&
  typeof tag[2] === "string"

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

/**
 * Removed device entry - tracks when a device was removed
 */
interface RemovedDevice {
  identityPubkey: string
  removedAt: number
}

/**
 * Manages a consolidated list of device invites (kind 30078, d-tag "double-ratchet/invite-list").
 * Single atomic event containing all device invites for a user.
 * Uses union merge strategy for conflict resolution.
 */
export class InviteList {
  private devices: Map<string, DeviceEntry> = new Map()
  private removedDevices: Map<string, RemovedDevice> = new Map()

  constructor(
    public readonly ownerPublicKey: string,
    devices: DeviceEntry[] = [],
    removedDevices: RemovedDevice[] = [],
  ) {
    this.removedDevices = new Map(removedDevices.map(r => [r.identityPubkey, r]))
    devices
      .filter((device) => !this.removedDevices.has(device.identityPubkey))
      .forEach((device) => this.devices.set(device.identityPubkey, device))
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
    if (this.removedDevices.has(device.identityPubkey)) {
      return
    }
    if (!this.devices.has(device.identityPubkey)) {
      this.devices.set(device.identityPubkey, device)
    }
  }

  removeDevice(identityPubkey: string): void {
    this.devices.delete(identityPubkey)
    this.removedDevices.set(identityPubkey, {
      identityPubkey,
      removedAt: now(),
    })
  }

  getDevice(identityPubkey: string): DeviceEntry | undefined {
    return this.devices.get(identityPubkey)
  }

  getAllDevices(): DeviceEntry[] {
    return Array.from(this.devices.values())
  }

  getRemovedDevices(): RemovedDevice[] {
    return Array.from(this.removedDevices.values())
  }

  getEvent(): UnsignedEvent {
    // Simplified tag format: ["device", identityPubkey, createdAt]
    const deviceTags = this.getAllDevices().map((device) => [
      "device",
      device.identityPubkey,
      String(device.createdAt),
    ])

    // Simplified removed tag format: ["removed", identityPubkey, removedAt]
    const removedTags = this.getRemovedDevices().map((removed) => [
      "removed",
      removed.identityPubkey,
      String(removed.removedAt),
    ])

    return {
      kind: INVITE_LIST_EVENT_KIND,
      pubkey: this.ownerPublicKey,
      content: "",
      created_at: now(),
      tags: [
        ["d", "double-ratchet/invite-list"],
        ["version", "3"], // Bump version for simplified format
        ...deviceTags,
        ...removedTags,
      ],
    }
  }

  static fromEvent(event: VerifiedEvent): InviteList {
    if (!event.sig) {
      throw new Error("Event is not signed")
    }
    if (!verifyEvent(event)) {
      throw new Error("Event signature is invalid")
    }

    // Simplified tag format: ["device", identityPubkey, createdAt]
    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, identityPubkey, createdAt]) => ({
        identityPubkey,
        createdAt: parseInt(createdAt, 10) || event.created_at,
      }))

    // Simplified removed tag format: ["removed", identityPubkey, removedAt]
    const removedDevices = event.tags
      .filter(isRemovedTag)
      .map(([, identityPubkey, removedAt]) => ({
        identityPubkey,
        removedAt: parseInt(removedAt, 10) || event.created_at,
      }))

    return new InviteList(event.pubkey, devices, removedDevices)
  }

  serialize(): string {
    return JSON.stringify({
      ownerPublicKey: this.ownerPublicKey,
      devices: this.getAllDevices(),
      removedDevices: this.getRemovedDevices(),
    })
  }

  static deserialize(json: string): InviteList {
    const data = JSON.parse(json) as {
      ownerPublicKey: string
      devices: DeviceEntry[]
      removedDevices?: RemovedDevice[]
    }
    return new InviteList(data.ownerPublicKey, data.devices, data.removedDevices || [])
  }

  merge(other: InviteList): InviteList {
    const mergedRemoved = new Map<string, RemovedDevice>()

    // Merge removed devices, keeping the earliest removal time
    for (const removed of [...this.removedDevices.values(), ...other.removedDevices.values()]) {
      const existing = mergedRemoved.get(removed.identityPubkey)
      if (!existing || removed.removedAt < existing.removedAt) {
        mergedRemoved.set(removed.identityPubkey, removed)
      }
    }

    // Merge devices, preferring the one with earlier createdAt for same identityPubkey
    const mergedDevices = [...this.devices.values(), ...other.devices.values()]
      .reduce((map, device) => {
        const existing = map.get(device.identityPubkey)
        if (!existing || device.createdAt < existing.createdAt) {
          map.set(device.identityPubkey, device)
        }
        return map
      }, new Map<string, DeviceEntry>())

    const activeDevices = Array.from(mergedDevices.values())
      .filter((device) => !mergedRemoved.has(device.identityPubkey))

    return new InviteList(
      this.ownerPublicKey,
      activeDevices,
      Array.from(mergedRemoved.values())
    )
  }
}
