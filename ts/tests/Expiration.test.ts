import { afterEach, describe, expect, it, vi } from "vitest"
import { generateSecretKey, getEventHash, getPublicKey } from "nostr-tools"
import { Session } from "../src/Session"
import { EXPIRATION_TAG } from "../src/types"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { GROUP_METADATA_KIND } from "../src/GroupMeta"

const FIXED_TIMESTAMP_MS = 1704067200000 // 2024-01-01 00:00:00 UTC

describe("Expiration / Disappearing Messages", () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("Session.send should add NIP-40-style expiration tag to the inner rumor", () => {
    vi.spyOn(Date, "now").mockReturnValue(FIXED_TIMESTAMP_MS)

    const aliceSecretKey = generateSecretKey()
    const bobSecretKey = generateSecretKey()

    const dummySubscribe = () => () => {}

    const alice = Session.init(
      dummySubscribe,
      getPublicKey(bobSecretKey),
      aliceSecretKey,
      true,
      new Uint8Array(),
      "alice"
    )

    const ttlSeconds = 60
    const expectedExpiresAt = Math.floor(FIXED_TIMESTAMP_MS / 1000) + ttlSeconds

    const { innerEvent } = alice.send("hi", { ttlSeconds })

    expect(innerEvent.tags).toContainEqual([EXPIRATION_TAG, String(expectedExpiresAt)])
    expect(innerEvent.id).toEqual(getEventHash(innerEvent))
  })

  it("SessionManager.sendMessage should propagate expiration tag to the receiver", async () => {
    vi.spyOn(Date, "now").mockReturnValue(FIXED_TIMESTAMP_MS)

    const sharedRelay = new MockRelay()

    const { manager: aliceManager } = await createMockSessionManager("alice-device-1", sharedRelay)
    const { manager: bobManager, publicKey: bobPubkey } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay
    )

    const ttlSeconds = 120
    const expectedExpiresAt = Math.floor(FIXED_TIMESTAMP_MS / 1000) + ttlSeconds

    const received = new Promise<string[][]>((resolve) => {
      bobManager.onEvent((event) => {
        if (event.content === "hello") {
          resolve(event.tags)
        }
      })
    })

    await aliceManager.sendMessage(bobPubkey, "hello", { ttlSeconds })

    const tags = await received
    expect(tags).toContainEqual([EXPIRATION_TAG, String(expectedExpiresAt)])
  })

  it("SessionManager should apply per-peer default expiration when caller doesn't pass one", async () => {
    vi.spyOn(Date, "now").mockReturnValue(FIXED_TIMESTAMP_MS)

    const sharedRelay = new MockRelay()

    const { manager: aliceManager } = await createMockSessionManager("alice-device-1", sharedRelay)
    const { manager: bobManager, publicKey: bobPubkey } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay
    )

    const ttlSeconds = 90
    const expectedExpiresAt = Math.floor(FIXED_TIMESTAMP_MS / 1000) + ttlSeconds

    await aliceManager.setExpirationForPeer(bobPubkey, { ttlSeconds })

    const received = new Promise<string[][]>((resolve) => {
      bobManager.onEvent((event) => {
        if (event.content === "hello-default") {
          resolve(event.tags)
        }
      })
    })

    await aliceManager.sendMessage(bobPubkey, "hello-default")

    const tags = await received
    expect(tags).toContainEqual([EXPIRATION_TAG, String(expectedExpiresAt)])
  })

  it("SessionManager should apply group default expiration based on ['l', <groupId>] tag", async () => {
    vi.spyOn(Date, "now").mockReturnValue(FIXED_TIMESTAMP_MS)

    const sharedRelay = new MockRelay()

    const { manager: aliceManager } = await createMockSessionManager("alice-device-1", sharedRelay)
    const { manager: bobManager, publicKey: bobPubkey } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay
    )

    const groupId = "test-group-1"
    const ttlSeconds = 30
    const expectedExpiresAt = Math.floor(FIXED_TIMESTAMP_MS / 1000) + ttlSeconds

    await aliceManager.setExpirationForGroup(groupId, { ttlSeconds })

    const received = new Promise<string[][]>((resolve) => {
      bobManager.onEvent((event) => {
        if (event.content === "hello-group-default") {
          resolve(event.tags)
        }
      })
    })

    await aliceManager.sendMessage(bobPubkey, "hello-group-default", { tags: [["l", groupId]] })

    const tags = await received
    expect(tags).toContainEqual([EXPIRATION_TAG, String(expectedExpiresAt)])
  })

  it("SessionManager should never add expiration to group metadata kind 40", async () => {
    vi.spyOn(Date, "now").mockReturnValue(FIXED_TIMESTAMP_MS)

    const sharedRelay = new MockRelay()
    const { manager: aliceManager, publicKey: alicePubkey } = await createMockSessionManager(
      "alice-device-1",
      sharedRelay
    )

    const groupId = "test-group-2"
    await aliceManager.setExpirationForGroup(groupId, { ttlSeconds: 10 })

    const rumor = await aliceManager.sendMessage(alicePubkey, "meta", {
      kind: GROUP_METADATA_KIND,
      tags: [["l", groupId]],
      ttlSeconds: 10,
    })

    expect(rumor.kind).toBe(GROUP_METADATA_KIND)
    expect(rumor.tags.some(([k]) => k === EXPIRATION_TAG)).toBe(false)
  })

  it("SessionManager should allow per-peer disabling of a global default expiration", async () => {
    vi.spyOn(Date, "now").mockReturnValue(FIXED_TIMESTAMP_MS)

    const sharedRelay = new MockRelay()

    const { manager: aliceManager } = await createMockSessionManager("alice-device-1", sharedRelay)
    const { manager: bobManager, publicKey: bobPubkey } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay
    )

    await aliceManager.setDefaultExpiration({ ttlSeconds: 60 })
    await aliceManager.setExpirationForPeer(bobPubkey, null)

    const received = new Promise<string[][]>((resolve) => {
      bobManager.onEvent((event) => {
        if (event.content === "hello-no-exp") {
          resolve(event.tags)
        }
      })
    })

    await aliceManager.sendMessage(bobPubkey, "hello-no-exp")
    const tags = await received
    expect(tags.some(([k]) => k === EXPIRATION_TAG)).toBe(false)
  })
})
