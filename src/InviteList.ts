import { finalizeEvent, VerifiedEvent, UnsignedEvent, verifyEvent } from "nostr-tools"
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

type DeviceTag = [
  type: "device",
  ephemeralPublicKey: string,
  sharedSecret: string,
  deviceId: string,
  deviceLabel: string,
  createdAt: string,
  ...rest: string[]  // Optional: identityPubkey at index 6
]

type RemovedTag = [type: "removed", deviceId: string]

const isDeviceTag = (tag: string[]): tag is DeviceTag =>
  tag.length >= 6 &&
  tag[0] === "device" &&
  tag.slice(1, 6).every((v) => typeof v === "string")

const isRemovedTag = (tag: string[]): tag is RemovedTag =>
  tag.length >= 2 &&
  tag[0] === "removed" &&
  typeof tag[1] === "string"

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
  /**
   * Identity public key for delegate devices.
   * If set, this device uses its own identity key for encryption/decryption
   * instead of the owner's main identity key.
   */
  identityPubkey?: string
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
    this.removedDeviceIds = new Set(removedDeviceIds)
    devices
      .filter((device) => !this.removedDeviceIds.has(device.deviceId))
      .forEach((device) => this.devices.set(device.deviceId, device))
  }

  /**
   * Updates a device's identity public key.
   * Useful for assigning a dedicated identity to the main device as well.
   */
  updateDeviceIdentityPubkey(deviceId: string, identityPubkey: string): void {
    const device = this.devices.get(deviceId)
    if (device) {
      device.identityPubkey = identityPubkey
    }
  }

  /**
   * Creates a new device entry with generated keys.
   * @param label - Human-readable label for the device
   * @param deviceId - Optional device ID (generates random if not provided)
   */
  createDevice(label: string, deviceId?: string): DeviceEntry {
    const keypair = generateEphemeralKeypair()
    return {
      ephemeralPublicKey: keypair.publicKey,
      ephemeralPrivateKey: keypair.privateKey,
      sharedSecret: generateSharedSecret(),
      deviceId: deviceId || generateDeviceId(),
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
    const deviceTags = this.getAllDevices().map((device) => {
      const tag = [
        "device",
        device.ephemeralPublicKey,
        device.sharedSecret,
        device.deviceId,
        device.deviceLabel,
        String(device.createdAt),
      ]
      // Only include identityPubkey if it's set (delegate devices)
      if (device.identityPubkey) {
        tag.push(device.identityPubkey)
      }
      return tag
    })

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
        ["version", "1"],
        ...deviceTags,
        ...removedTags,
      ],
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

    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, ephemeralPublicKey, sharedSecret, deviceId, deviceLabel, createdAt, identityPubkey]) => ({
        ephemeralPublicKey,
        sharedSecret,
        deviceId,
        deviceLabel,
        createdAt: parseInt(createdAt, 10) || event.created_at,
        identityPubkey: identityPubkey || undefined,
      }))

    const removedDeviceIds = event.tags
      .filter(isRemovedTag)
      .map(([, deviceId]) => deviceId)

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
    const mergedRemovedIds = new Set([
      ...this.removedDeviceIds,
      ...other.removedDeviceIds,
    ])

    // Union all devices, preserving private keys from either list
    const mergedDevices = [...this.devices.values(), ...other.devices.values()]
      .reduce((map, device) => {
        const existing = map.get(device.deviceId)
        map.set(device.deviceId, existing
          ? { ...device, ephemeralPrivateKey: existing.ephemeralPrivateKey || device.ephemeralPrivateKey }
          : device
        )
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

    // For delegate devices, use the delegate's identity key for DH encryption
    // For regular devices, use the InviteList owner's public key
    const inviterIdentityKey = device.identityPubkey ?? this.ownerPublicKey

    const encrypted = await encryptInviteResponse({
      inviteeSessionPublicKey: inviteeSessionKeypair.publicKey,
      inviteePublicKey,
      inviteePrivateKey,
      inviterPublicKey: inviterIdentityKey,
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
    const decryptableDevices = devices.filter((d) => !!d.ephemeralPrivateKey)

    // If we don't have any devices we can decrypt for, do nothing gracefully
    if (decryptableDevices.length === 0) {
      return () => {}
    }

    const ephemeralPubkeys = decryptableDevices.map((d) => d.ephemeralPublicKey)

    const filter = {
      kinds: [INVITE_RESPONSE_KIND],
      "#p": ephemeralPubkeys,
    }

    const decrypt = typeof decryptor === "function" ? decryptor : undefined
    const ownerPrivateKey = typeof decryptor === "function" ? undefined : decryptor

    return nostrSubscribe(filter, async (event) => {
      // Find which device this response is for
      const targetPubkey = event.tags.find((t) => t[0] === "p")?.[1]
      const device = decryptableDevices.find((d) => d.ephemeralPublicKey === targetPubkey)

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
