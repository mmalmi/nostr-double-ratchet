import { VerifiedEvent, UnsignedEvent, verifyEvent } from "nostr-tools"
import { INVITE_LIST_EVENT_KIND, NostrSubscribe, Unsubscribe } from "./types"

const now = () => Math.round(Date.now() / 1000)

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
 * Manages a consolidated list of device invites (kind 30078, d-tag "double-ratchet/invite-list").
 * Single atomic event containing all device invites for a user.
 * Uses union merge strategy for conflict resolution.
 *
 * Note: ownerPublicKey is not stored - it's passed to getEvent() when publishing,
 * and NDK's signer sets the correct pubkey during signing anyway.
 */
export class InviteList {
  private devices: Map<string, DeviceEntry> = new Map()

  constructor(devices: DeviceEntry[] = []) {
    devices.forEach((device) => this.devices.set(device.identityPubkey, device))
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
  }

  getDevice(identityPubkey: string): DeviceEntry | undefined {
    return this.devices.get(identityPubkey)
  }

  getAllDevices(): DeviceEntry[] {
    return Array.from(this.devices.values())
  }

  getEvent(): UnsignedEvent {
    const deviceTags = this.getAllDevices().map((device) => [
      "device",
      device.identityPubkey,
      String(device.createdAt),
    ])

    return {
      kind: INVITE_LIST_EVENT_KIND,
      pubkey: "", // Signer will set this
      content: "",
      created_at: now(),
      tags: [
        ["d", "double-ratchet/invite-list"],
        ["version", "1"],
        ...deviceTags,
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
    // Note: "removed" tags are ignored for backwards compatibility with old events
    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, identityPubkey, createdAt]) => ({
        identityPubkey,
        createdAt: parseInt(createdAt, 10) || event.created_at,
      }))

    return new InviteList(devices)
  }

  serialize(): string {
    return JSON.stringify({
      devices: this.getAllDevices(),
    })
  }

  static deserialize(json: string): InviteList {
    const data = JSON.parse(json) as {
      devices: DeviceEntry[]
    }
    return new InviteList(data.devices)
  }

  merge(other: InviteList): InviteList {
    // Merge devices, preferring the one with earlier createdAt for same identityPubkey
    const mergedDevices = [...this.devices.values(), ...other.devices.values()]
      .reduce((map, device) => {
        const existing = map.get(device.identityPubkey)
        if (!existing || device.createdAt < existing.createdAt) {
          map.set(device.identityPubkey, device)
        }
        return map
      }, new Map<string, DeviceEntry>())

    return new InviteList(Array.from(mergedDevices.values()))
  }

  /**
   * Subscribe to InviteList events from a user.
   * Similar to Invite.fromUser pattern.
   */
  static fromUser(
    user: string,
    subscribe: NostrSubscribe,
    onInviteList: (inviteList: InviteList) => void
  ): Unsubscribe {
    return subscribe(
      {
        kinds: [INVITE_LIST_EVENT_KIND],
        authors: [user],
        "#d": ["double-ratchet/invite-list"],
      },
      (event) => {
        if (event.pubkey !== user) return
        try {
          const inviteList = InviteList.fromEvent(event)
          onInviteList(inviteList)
        } catch {
          // Invalid event
        }
      }
    )
  }

  /**
   * Wait for InviteList from a user with timeout.
   * Returns the most recent InviteList received within the timeout, or null.
   * Note: Uses the most recent event by created_at, not merging, since
   * device revocation is determined by absence from the list.
   */
  static waitFor(
    user: string,
    subscribe: NostrSubscribe,
    timeoutMs = 500
  ): Promise<InviteList | null> {
    return new Promise((resolve) => {
      let latest: { list: InviteList; createdAt: number } | null = null

      setTimeout(() => {
        unsubscribe()
        resolve(latest?.list ?? null)
      }, timeoutMs)

      const unsubscribe = subscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          authors: [user],
          "#d": ["double-ratchet/invite-list"],
        },
        (event) => {
          if (event.pubkey !== user) return
          try {
            const list = InviteList.fromEvent(event)
            if (!latest || event.created_at > latest.createdAt) {
              latest = { list, createdAt: event.created_at }
            }
          } catch {
            // Invalid event
          }
        }
      )
    })
  }
}
