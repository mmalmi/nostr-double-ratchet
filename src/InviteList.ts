import { finalizeEvent, VerifiedEvent, UnsignedEvent, verifyEvent } from "nostr-tools"
import { hexToBytes, bytesToHex } from "@noble/hashes/utils"
import {
  NostrSubscribe,
  Unsubscribe,
  EncryptFunction,
  DecryptFunction,
  INVITE_LIST_EVENT_KIND,
  INVITE_RESPONSE_KIND,
} from "./types"
import { Session } from "./Session"
import {
  generateEphemeralKeypair,
  generateSharedSecret,
  generateDeviceId,
  encryptInviteResponse,
  decryptInviteResponse,
  createSessionFromAccept,
} from "./inviteUtils"

const now = () => Math.round(Date.now() / 1000)

/**
 * A device entry in the invite list.
 */
export interface DeviceEntry {
  /** Ephemeral public key for this device (used for handshakes) */
  ephemeralPublicKey: string
  /** Ephemeral private key (only stored locally, not published) */
  ephemeralPrivateKey?: Uint8Array
  /** Shared secret for initial handshake encryption */
  sharedSecret: string
  /** Unique identifier for this device */
  deviceId: string
  /** Human-readable label (e.g., "iPhone", "Laptop") */
  deviceLabel: string
  /** When this device was added (unix timestamp) */
  createdAt: number
}

/**
 * InviteList manages a consolidated list of device invites (kind 10078).
 *
 * This replaces the per-device invite approach with a single atomic event
 * containing all device invites for a user.
 *
 * Features:
 * - Atomic updates across all devices
 * - Device revocation by any device with main nsec
 * - Single query to fetch all device invites
 * - Union merge strategy for conflict resolution
 */
export class InviteList {
  private devices: Map<string, DeviceEntry> = new Map()
  private removedDeviceIds: Set<string> = new Set()

  constructor(
    public readonly ownerPublicKey: string,
    devices: DeviceEntry[] = [],
    removedDeviceIds: string[] = [],
  ) {
    for (const deviceId of removedDeviceIds) {
      this.removedDeviceIds.add(deviceId)
    }
    for (const device of devices) {
      if (!this.removedDeviceIds.has(device.deviceId)) {
        this.devices.set(device.deviceId, device)
      }
    }
  }

  /**
   * Creates a new device entry with generated keys.
   */
  createDevice(label: string): DeviceEntry {
    const keypair = generateEphemeralKeypair()
    return {
      ephemeralPublicKey: keypair.publicKey,
      ephemeralPrivateKey: keypair.privateKey,
      sharedSecret: generateSharedSecret(),
      deviceId: generateDeviceId(),
      deviceLabel: label,
      createdAt: now(),
    }
  }

  /**
   * Adds a device to the list. Does nothing if device ID is already present or was removed.
   */
  addDevice(device: DeviceEntry): void {
    if (this.removedDeviceIds.has(device.deviceId)) {
      return // Cannot re-add a removed device
    }
    if (!this.devices.has(device.deviceId)) {
      this.devices.set(device.deviceId, device)
    }
  }

  /**
   * Removes a device from the list. The device ID is tracked to prevent re-addition.
   */
  removeDevice(deviceId: string): void {
    this.devices.delete(deviceId)
    this.removedDeviceIds.add(deviceId)
  }

  /**
   * Gets a device by its ID.
   */
  getDevice(deviceId: string): DeviceEntry | undefined {
    return this.devices.get(deviceId)
  }

  /**
   * Gets all active devices.
   */
  getAllDevices(): DeviceEntry[] {
    return Array.from(this.devices.values())
  }

  /**
   * Gets all removed device IDs.
   */
  getRemovedDeviceIds(): string[] {
    return Array.from(this.removedDeviceIds)
  }

  /**
   * Updates a device's label.
   */
  updateDeviceLabel(deviceId: string, newLabel: string): void {
    const device = this.devices.get(deviceId)
    if (device) {
      device.deviceLabel = newLabel
    }
  }

  /**
   * Creates an unsigned event representing this invite list.
   */
  getEvent(): UnsignedEvent {
    const tags: string[][] = [
      ["d", "double-ratchet/invite-list"],
      ["version", "1"],
    ]

    // Add device tags
    for (const device of this.devices.values()) {
      tags.push([
        "device",
        device.ephemeralPublicKey,
        device.sharedSecret,
        device.deviceId,
        device.deviceLabel,
        String(device.createdAt),
      ])
    }

    // Add removed device tags
    for (const deviceId of this.removedDeviceIds) {
      tags.push(["removed", deviceId])
    }

    return {
      kind: INVITE_LIST_EVENT_KIND,
      pubkey: this.ownerPublicKey,
      content: "",
      created_at: now(),
      tags,
    }
  }

  /**
   * Parses an InviteList from a signed Nostr event.
   */
  static fromEvent(event: VerifiedEvent): InviteList {
    if (!event.sig) {
      throw new Error("Event is not signed")
    }
    if (!verifyEvent(event)) {
      throw new Error("Event signature is invalid")
    }

    const devices: DeviceEntry[] = []
    const removedDeviceIds: string[] = []

    for (const tag of event.tags) {
      if (tag[0] === "device" && tag.length >= 5) {
        devices.push({
          ephemeralPublicKey: tag[1],
          sharedSecret: tag[2],
          deviceId: tag[3],
          deviceLabel: tag[4],
          createdAt: tag[5] ? parseInt(tag[5], 10) : event.created_at,
        })
      } else if (tag[0] === "removed" && tag.length >= 2) {
        removedDeviceIds.push(tag[1])
      }
    }

    return new InviteList(event.pubkey, devices, removedDeviceIds)
  }

  /**
   * Serializes the invite list to JSON for local storage.
   * Includes private keys.
   */
  serialize(): string {
    const devices = this.getAllDevices().map((d) => ({
      ...d,
      ephemeralPrivateKey: d.ephemeralPrivateKey
        ? Array.from(d.ephemeralPrivateKey)
        : undefined,
    }))

    return JSON.stringify({
      ownerPublicKey: this.ownerPublicKey,
      devices,
      removedDeviceIds: this.getRemovedDeviceIds(),
    })
  }

  /**
   * Deserializes an InviteList from JSON.
   */
  static deserialize(json: string): InviteList {
    const data = JSON.parse(json)
    const devices: DeviceEntry[] = data.devices.map((d: any) => ({
      ...d,
      ephemeralPrivateKey: d.ephemeralPrivateKey
        ? new Uint8Array(d.ephemeralPrivateKey)
        : undefined,
    }))

    return new InviteList(data.ownerPublicKey, devices, data.removedDeviceIds || [])
  }

  /**
   * Merges another InviteList into this one using union strategy.
   *
   * - Union all devices from both lists
   * - Union all removed device IDs
   * - Active devices = all devices − removed devices
   * - Private keys are preserved from the list that has them
   */
  merge(other: InviteList): InviteList {
    // Union removed device IDs
    const mergedRemovedIds = new Set([
      ...this.removedDeviceIds,
      ...other.removedDeviceIds,
    ])

    // Union all devices, preserving private keys
    const mergedDevices = new Map<string, DeviceEntry>()

    // Add devices from this list
    for (const device of this.devices.values()) {
      mergedDevices.set(device.deviceId, device)
    }

    // Add/merge devices from other list
    for (const device of other.devices.values()) {
      const existing = mergedDevices.get(device.deviceId)
      if (existing) {
        // Preserve private key from whichever has it
        mergedDevices.set(device.deviceId, {
          ...device,
          ephemeralPrivateKey:
            existing.ephemeralPrivateKey || device.ephemeralPrivateKey,
        })
      } else {
        mergedDevices.set(device.deviceId, device)
      }
    }

    // Filter out removed devices
    const activeDevices: DeviceEntry[] = []
    for (const device of mergedDevices.values()) {
      if (!mergedRemovedIds.has(device.deviceId)) {
        activeDevices.push(device)
      }
    }

    return new InviteList(
      this.ownerPublicKey,
      activeDevices,
      Array.from(mergedRemovedIds)
    )
  }

  /**
   * Called by an invitee to accept an invite from a specific device.
   *
   * @param deviceId - The device ID to accept the invite from
   * @param nostrSubscribe - Nostr subscription function
   * @param inviteePublicKey - The invitee's public key
   * @param encryptor - The invitee's private key or encrypt function
   * @param inviteeDeviceId - Optional device ID for the invitee
   * @returns The session and event to publish
   */
  async accept(
    deviceId: string,
    nostrSubscribe: NostrSubscribe,
    inviteePublicKey: string,
    encryptor: Uint8Array | EncryptFunction,
    inviteeDeviceId?: string
  ): Promise<{ session: Session; event: VerifiedEvent }> {
    const device = this.devices.get(deviceId)
    if (!device) {
      throw new Error(`Device ${deviceId} not found in invite list`)
    }

    const inviteeSessionKeypair = generateEphemeralKeypair()

    const session = createSessionFromAccept({
      nostrSubscribe,
      theirPublicKey: device.ephemeralPublicKey,
      ourSessionPrivateKey: inviteeSessionKeypair.privateKey,
      sharedSecret: device.sharedSecret,
      isSender: true,
    })

    const encrypt = typeof encryptor === "function" ? encryptor : undefined
    const inviteePrivateKey = typeof encryptor === "function" ? undefined : encryptor

    const encrypted = await encryptInviteResponse({
      inviteeSessionPublicKey: inviteeSessionKeypair.publicKey,
      inviteePublicKey,
      inviteePrivateKey,
      inviterPublicKey: this.ownerPublicKey,
      inviterEphemeralPublicKey: device.ephemeralPublicKey,
      sharedSecret: device.sharedSecret,
      deviceId: inviteeDeviceId,
      encrypt,
    })

    return {
      session,
      event: finalizeEvent(encrypted.envelope, encrypted.randomSenderPrivateKey),
    }
  }

  /**
   * Listens for invite responses on all devices.
   *
   * @param decryptor - The owner's private key or decrypt function
   * @param nostrSubscribe - Nostr subscription function
   * @param onSession - Callback when a new session is established
   * @returns Unsubscribe function
   */
  listen(
    decryptor: Uint8Array | DecryptFunction,
    nostrSubscribe: NostrSubscribe,
    onSession: (
      session: Session,
      identity: string,
      deviceId?: string,
      ourDeviceId?: string
    ) => void
  ): Unsubscribe {
    const devices = this.getAllDevices()

    // Verify all devices have private keys
    for (const device of devices) {
      if (!device.ephemeralPrivateKey) {
        throw new Error(
          `Device ${device.deviceId} does not have ephemeral private key. Cannot listen for responses.`
        )
      }
    }

    if (devices.length === 0) {
      return () => {}
    }

    // Subscribe to responses for all device ephemeral keys
    const ephemeralPubkeys = devices.map((d) => d.ephemeralPublicKey)

    const filter = {
      kinds: [INVITE_RESPONSE_KIND],
      "#p": ephemeralPubkeys,
    }

    const decrypt = typeof decryptor === "function" ? decryptor : undefined
    const ownerPrivateKey = typeof decryptor === "function" ? undefined : decryptor

    return nostrSubscribe(filter, async (event) => {
      // Find which device this response is for
      const targetPubkey = event.tags.find((t) => t[0] === "p")?.[1]
      const device = devices.find((d) => d.ephemeralPublicKey === targetPubkey)

      if (!device || !device.ephemeralPrivateKey) {
        return
      }

      try {
        const decrypted = await decryptInviteResponse({
          envelopeContent: event.content,
          envelopeSenderPubkey: event.pubkey,
          inviterEphemeralPrivateKey: device.ephemeralPrivateKey,
          inviterPrivateKey: ownerPrivateKey,
          sharedSecret: device.sharedSecret,
          decrypt,
        })

        const session = createSessionFromAccept({
          nostrSubscribe,
          theirPublicKey: decrypted.inviteeSessionPublicKey,
          ourSessionPrivateKey: device.ephemeralPrivateKey,
          sharedSecret: device.sharedSecret,
          isSender: false,
          name: event.id,
        })

        onSession(
          session,
          decrypted.inviteeIdentity,
          decrypted.deviceId,
          device.deviceId
        )
      } catch {
        // Invalid response, ignore
      }
    })
  }
}
