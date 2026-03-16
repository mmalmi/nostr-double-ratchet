import { describe, expect, it, vi } from "vitest"
import {
  finalizeEvent,
  generateSecretKey,
  getPublicKey,
  type UnsignedEvent,
  type VerifiedEvent,
} from "nostr-tools"
import { APP_KEYS_EVENT_KIND } from "../src/types"
import { AppKeys } from "../src/AppKeys"
import { Invite } from "../src/Invite"
import { generateEphemeralKeypair, generateSharedSecret, decryptInviteResponse } from "../src/inviteUtils"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { SessionManager } from "../src/SessionManager"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"

function extractInviteForOwner(relay: MockRelay, ownerPubkey: string): Invite {
  const events = relay.getAllEvents()

  const appKeysEvent = events.find(
    (event) =>
      event.kind === APP_KEYS_EVENT_KIND &&
      event.pubkey === ownerPubkey &&
      event.tags.some((tag) => tag[0] === "d" && tag[1] === "double-ratchet/app-keys")
  ) as VerifiedEvent | undefined
  if (!appKeysEvent) {
    throw new Error("No AppKeys event found for owner")
  }

  const appKeys = AppKeys.fromEvent(appKeysEvent)
  const deviceIdentity = appKeys.getAllDevices()[0]?.identityPubkey
  if (!deviceIdentity) {
    throw new Error("No device identity found in AppKeys")
  }

  const inviteEvent = events.find(
    (event) =>
      event.kind === 30078 &&
      event.pubkey === deviceIdentity &&
      event.tags.some((tag) => tag[0] === "l" && tag[1] === "double-ratchet/invites")
  ) as VerifiedEvent | undefined
  if (!inviteEvent) {
    throw new Error("No invite event found for device")
  }

  const invite = Invite.fromEvent(inviteEvent)
  invite.ownerPubkey = ownerPubkey
  return invite
}

describe("SessionManager.acceptInvite", () => {
  const createRelaySubscribe = (relay: MockRelay) => (filter: Parameters<MockRelay["subscribe"]>[0], onEvent: Parameters<MockRelay["subscribe"]>[1]) => {
    const handle = relay.subscribe(filter, onEvent)
    return handle.close
  }

  const createRelayPublish =
    (relay: MockRelay, signerSecretKey: Uint8Array) =>
    vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      const signedEvent =
        "sig" in event && event.sig
          ? (event as VerifiedEvent)
          : (finalizeEvent(event as UnsignedEvent, signerSecretKey) as VerifiedEvent)
      relay.storeAndDeliver(signedEvent)
      return signedEvent as never
    })

  it("establishes manager session from invite and routes under owner pubkey", async () => {
    const relay = new MockRelay()

    const bob = await createMockSessionManager("bob-device-1", relay)
    const alice = await createMockSessionManager("alice-device-1", relay)

    const invite = extractInviteForOwner(relay, bob.publicKey)

    const accepted = await alice.manager.acceptInvite(invite, {
      ownerPublicKey: bob.publicKey,
    })

    expect(accepted.ownerPublicKey).toBe(bob.publicKey)
    expect(accepted.deviceId).toBe(invite.deviceId || invite.inviter)

    const bobRecord = alice.manager.getUserRecords().get(bob.publicKey)
    expect(bobRecord).toBeDefined()
    expect(Array.from(bobRecord!.devices.keys())).toContain(accepted.deviceId)

    const text = `hello-from-accept-invite-${Date.now()}`
    const bobReceived = new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error("Timed out waiting for Bob to receive accepted-invite message")),
        10_000
      )
      const unsubscribe = bob.manager.onEvent((event) => {
        if (event.content !== text) return
        clearTimeout(timeout)
        unsubscribe()
        resolve()
      })
    })

    await alice.manager.sendMessage(bob.publicKey, text)
    await bobReceived
  })

  it("accepts link invite for owner even before new device appears in owner AppKeys", async () => {
    const relay = new MockRelay()

    const owner = await createMockSessionManager("owner-device-1", relay)
    const newDevicePubkey = getPublicKey(generateSecretKey())

    const invite = Invite.createNew(newDevicePubkey, newDevicePubkey, 1, {
      purpose: "link",
      ownerPubkey: owner.publicKey,
    })

    const accepted = await owner.manager.acceptInvite(invite, {
      ownerPublicKey: owner.publicKey,
    })

    expect(accepted.ownerPublicKey).toBe(owner.publicKey)
    expect(accepted.deviceId).toBe(newDevicePubkey)

    const ownerRecord = owner.manager.getUserRecords().get(owner.publicKey)
    expect(ownerRecord).toBeDefined()
    expect(Array.from(ownerRecord!.devices.keys())).toContain(newDevicePubkey)

    const bootstrapEvents = relay
      .getAllEvents()
      .filter((event) => event.kind === 1060)
    expect(bootstrapEvents.length).toBeGreaterThan(0)
  })

  it("falls back to inviter device identity when chat owner claim is not authorized by AppKeys", async () => {
    const relay = new MockRelay()

    const bob = await createMockSessionManager("bob-device-1", relay)
    const alice = await createMockSessionManager("alice-device-1", relay)
    const unknownBobDevicePubkey = getPublicKey(generateSecretKey())

    const invite = Invite.createNew(unknownBobDevicePubkey, unknownBobDevicePubkey, 1, {
      purpose: "chat",
      ownerPubkey: bob.publicKey,
    })

    const accepted = await alice.manager.acceptInvite(invite, {
      ownerPublicKey: bob.publicKey,
    })

    expect(accepted.ownerPublicKey).toBe(unknownBobDevicePubkey)
    expect(accepted.deviceId).toBe(unknownBobDevicePubkey)

    const bobRecord = alice.manager.getUserRecords().get(bob.publicKey)
    const bobRecordHasUnknownDevice =
      !!bobRecord &&
      Array.from(bobRecord.devices.values()).some(
        (device) => device.deviceId === unknownBobDevicePubkey
      )
    expect(bobRecordHasUnknownDevice).toBe(false)

    const fallbackRecord = alice.manager.getUserRecords().get(unknownBobDevicePubkey)
    expect(fallbackRecord).toBeDefined()
    expect(Array.from(fallbackRecord!.devices.keys())).toContain(unknownBobDevicePubkey)
  })

  it("omits our owner claim in invite responses until this device is authorized in AppKeys", async () => {
    const relay = new MockRelay()

    const bob = await createMockSessionManager("bob-device-1", relay)
    const invite = Invite.createNew(bob.publicKey, bob.publicKey, 1)

    const ownerSecretKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerSecretKey)
    const deviceSecretKey = generateSecretKey()
    const devicePublicKey = getPublicKey(deviceSecretKey)

    const alice = new SessionManager(
      devicePublicKey,
      deviceSecretKey,
      devicePublicKey,
      createRelaySubscribe(relay),
      createRelayPublish(relay, deviceSecretKey),
      ownerPublicKey,
      {
        ephemeralKeypair: generateEphemeralKeypair(),
        sharedSecret: generateSharedSecret(),
      },
      new InMemoryStorageAdapter()
    )
    await alice.init()

    await alice.acceptInvite(invite, {
      ownerPublicKey: bob.publicKey,
    })

    const responseEvent = relay
      .getAllEvents()
      .filter((event) => event.kind === 1059)
      .at(-1)
    expect(responseEvent).toBeDefined()

    const decrypted = await decryptInviteResponse({
      envelopeContent: responseEvent!.content,
      envelopeSenderPubkey: responseEvent!.pubkey,
      inviterEphemeralPrivateKey: invite.inviterEphemeralPrivateKey!,
      inviterPrivateKey: bob.secretKey,
      sharedSecret: invite.sharedSecret,
    })

    expect(decrypted.inviteeIdentity).toBe(devicePublicKey)
    expect(decrypted.ownerPublicKey).toBeUndefined()
  })

  it("includes our owner claim in invite responses once this device is authorized in AppKeys", async () => {
    const relay = new MockRelay()

    const bob = await createMockSessionManager("bob-device-1", relay)
    const alice = await createMockSessionManager("alice-device-1", relay)
    const invite = Invite.createNew(bob.publicKey, bob.publicKey, 1)

    await alice.manager.acceptInvite(invite, {
      ownerPublicKey: bob.publicKey,
    })

    const responseEvent = relay
      .getAllEvents()
      .filter((event) => event.kind === 1059)
      .at(-1)
    expect(responseEvent).toBeDefined()

    const decrypted = await decryptInviteResponse({
      envelopeContent: responseEvent!.content,
      envelopeSenderPubkey: responseEvent!.pubkey,
      inviterEphemeralPrivateKey: invite.inviterEphemeralPrivateKey!,
      inviterPrivateKey: bob.secretKey,
      sharedSecret: invite.sharedSecret,
    })

    const aliceDeviceIdentity = alice.appKeysManager.getOwnDevices()[0]?.identityPubkey
    expect(aliceDeviceIdentity).toBeTruthy()
    expect(decrypted.inviteeIdentity).toBe(aliceDeviceIdentity)
    expect(decrypted.ownerPublicKey).toBe(alice.publicKey)
  })
})
