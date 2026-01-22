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
  createdAt: string,
  identityPubkey: string,
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

export interface DeviceEntry {
  ephemeralPublicKey: string
  /** Only stored locally, not published */
  ephemeralPrivateKey?: Uint8Array
  sharedSecret: string
  deviceId: string
  deviceLabel: string
  createdAt: number
  /** Owner's pubkey for owner devices, delegate's own pubkey for delegate devices */
  identityPubkey: string
}

interface SerializedDeviceEntry extends Omit<DeviceEntry, 'ephemeralPrivateKey'> {
  ephemeralPrivateKey?: number[]
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

  createDevice(label: string, deviceId?: string): DeviceEntry {
    const keypair = generateEphemeralKeypair()
    return {
      ephemeralPublicKey: keypair.publicKey,
      ephemeralPrivateKey: keypair.privateKey,
      sharedSecret: generateSharedSecret(),
      deviceId: deviceId || generateDeviceId(),
      deviceLabel: label,
      createdAt: now(),
      identityPubkey: this.ownerPublicKey,
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
    const deviceTags = this.getAllDevices().map((device) => [
      "device",
      device.ephemeralPublicKey,
      device.sharedSecret,
      device.deviceId,
      String(device.createdAt),
      device.identityPubkey,
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
        ["version", "1"],
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

    const devices = event.tags
      .filter(isDeviceTag)
      .map(([, ephemeralPublicKey, sharedSecret, deviceId, createdAt, identityPubkey]) => ({
        ephemeralPublicKey,
        sharedSecret,
        deviceId,
        deviceLabel: deviceId,
        createdAt: parseInt(createdAt, 10) || event.created_at,
        identityPubkey,
      }))

    const removedDeviceIds = event.tags
      .filter(isRemovedTag)
      .map(([, deviceId]) => deviceId)

    return new InviteList(event.pubkey, devices, removedDeviceIds)
  }

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

  static deserialize(json: string): InviteList {
    const data = JSON.parse(json) as { ownerPublicKey: string; devices: SerializedDeviceEntry[]; removedDeviceIds?: string[] }
    const devices: DeviceEntry[] = data.devices.map((d) => ({
      ...d,
      ephemeralPrivateKey: d.ephemeralPrivateKey
        ? new Uint8Array(d.ephemeralPrivateKey)
        : undefined,
    }))

    return new InviteList(data.ownerPublicKey, devices, data.removedDeviceIds || [])
  }

  merge(other: InviteList): InviteList {
    const mergedRemovedIds = new Set([
      ...this.removedDeviceIds,
      ...other.removedDeviceIds,
    ])

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
      inviterPublicKey: device.identityPubkey,
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
    const decrypt = typeof decryptor === "function" ? decryptor : undefined
    const ownerPrivateKey = typeof decryptor === "function" ? undefined : decryptor

    return nostrSubscribe(
      { kinds: [INVITE_RESPONSE_KIND], "#p": ephemeralPubkeys },
      async (event) => {
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

          onSession(session, decrypted.inviteeIdentity, decrypted.deviceId, device.deviceId)
        } catch {
          // Invalid response
        }
      }
    )
  }
}
