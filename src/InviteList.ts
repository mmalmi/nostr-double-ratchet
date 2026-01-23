import { VerifiedEvent, UnsignedEvent, verifyEvent } from "nostr-tools"
import { INVITE_LIST_EVENT_KIND } from "./types"
import { generateDeviceId } from "./inviteUtils"

const now = () => Math.round(Date.now() / 1000)

// New tag format: ["device", deviceId, identityPubkey, createdAt]
type DeviceTag = [
  type: "device",
  deviceId: string,
  identityPubkey: string,
  createdAt: string,
]

type RemovedTag = [type: "removed", deviceId: string]

const isDeviceTag = (tag: string[]): tag is DeviceTag =>
  tag.length >= 4 &&
  tag[0] === "device" &&
  tag.slice(1, 4).every((v) => typeof v === "string")

const isRemovedTag = (tag: string[]): tag is RemovedTag =>
  tag.length >= 2 &&
  tag[0] === "removed" &&
  typeof tag[1] === "string"

/**
 * Device identity entry - contains only identity information.
 * Invite crypto material (ephemeral keys, shared secret) is now in separate Invite events.
 */
export interface DeviceEntry {
  deviceId: string
  /** Human-readable device label (stored locally only, not published) */
  deviceLabel: string
  createdAt: number
  /** Owner's pubkey for owner devices, delegate's own pubkey for delegate devices */
  identityPubkey: string
}

/**
 * Manages a consolidated list of device invites (kind 30078, d-tag "double-ratchet/invite-list").
 * Single atomic event containing all device invites for a user.
 * Uses union merge strategy for conflict resolution.
 */
export class InviteList {
  private devices: Map<string, DeviceEntry> = new Map()
  private removedDeviceIds: Set<string> = new Set()

  constructor(
    public readonly ownerPublicKey: string,
    devices: DeviceEntry[] = [],
    removedDeviceIds: string[] = [],
  ) {
    this.removedDeviceIds = new Set(removedDeviceIds)
    devices
      .filter((device) => !this.removedDeviceIds.has(device.deviceId))
      .forEach((device) => this.devices.set(device.deviceId, device))
  }

  /**
   * Creates a new device identity entry.
   * Note: This only creates the identity entry. The device must separately
   * create and publish its own Invite event with ephemeral keys.
   */
  createDeviceEntry(label: string, identityPubkey: string, deviceId?: string): DeviceEntry {
    return {
      deviceId: deviceId || generateDeviceId(),
      deviceLabel: label,
      createdAt: now(),
      identityPubkey,
    }
  }

  addDevice(device: DeviceEntry): void {
    if (this.removedDeviceIds.has(device.deviceId)) {
      return
    }
    if (!this.devices.has(device.deviceId)) {
      this.devices.set(device.deviceId, device)
    }
  }

  removeDevice(deviceId: string): void {
    this.devices.delete(deviceId)
    this.removedDeviceIds.add(deviceId)
  }

  getDevice(deviceId: string): DeviceEntry | undefined {
    return this.devices.get(deviceId)
  }

  getAllDevices(): DeviceEntry[] {
    return Array.from(this.devices.values())
  }

  getRemovedDeviceIds(): string[] {
    return Array.from(this.removedDeviceIds)
  }

  updateDeviceLabel(deviceId: string, newLabel: string): void {
    const device = this.devices.get(deviceId)
    if (device) {
      device.deviceLabel = newLabel
    }
  }

  getEvent(): UnsignedEvent {
    // New tag format: ["device", deviceId, identityPubkey, createdAt]
    const deviceTags = this.getAllDevices().map((device) => [
      "device",
      device.deviceId,
      device.identityPubkey,
      String(device.createdAt),
    ])

    const removedTags = this.getRemovedDeviceIds().map((deviceId) => [
      "removed",
      deviceId,
    ])

    return {
      kind: INVITE_LIST_EVENT_KIND,
      pubkey: this.ownerPublicKey,
      content: "",
      created_at: now(),
      tags: [
        ["d", "double-ratchet/invite-list"],
        ["version", "2"], // Bump version for new format
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

    // New tag format: ["device", deviceId, identityPubkey, createdAt]
    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, deviceId, identityPubkey, createdAt]) => ({
        deviceId,
        deviceLabel: deviceId, // Use deviceId as default label (actual label stored locally)
        createdAt: parseInt(createdAt, 10) || event.created_at,
        identityPubkey,
      }))

    const removedDeviceIds = event.tags
      .filter(isRemovedTag)
      .map(([, deviceId]) => deviceId)

    return new InviteList(event.pubkey, devices, removedDeviceIds)
  }

  serialize(): string {
    return JSON.stringify({
      ownerPublicKey: this.ownerPublicKey,
      devices: this.getAllDevices(),
      removedDeviceIds: this.getRemovedDeviceIds(),
    })
  }

  static deserialize(json: string): InviteList {
    const data = JSON.parse(json) as {
      ownerPublicKey: string
      devices: DeviceEntry[]
      removedDeviceIds?: string[]
    }
    return new InviteList(data.ownerPublicKey, data.devices, data.removedDeviceIds || [])
  }

  merge(other: InviteList): InviteList {
    const mergedRemovedIds = new Set([
      ...this.removedDeviceIds,
      ...other.removedDeviceIds,
    ])

    // Merge devices, preferring the one with earlier createdAt for same deviceId
    const mergedDevices = [...this.devices.values(), ...other.devices.values()]
      .reduce((map, device) => {
        const existing = map.get(device.deviceId)
        if (!existing || device.createdAt < existing.createdAt) {
          map.set(device.deviceId, device)
        }
        return map
      }, new Map<string, DeviceEntry>())

    const activeDevices = Array.from(mergedDevices.values())
      .filter((device) => !mergedRemovedIds.has(device.deviceId))

    return new InviteList(
      this.ownerPublicKey,
      activeDevices,
      Array.from(mergedRemovedIds)
    )
  }
}
