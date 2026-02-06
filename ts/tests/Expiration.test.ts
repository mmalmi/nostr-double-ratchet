import { afterEach, describe, expect, it, vi } from "vitest"
import { generateSecretKey, getEventHash, getPublicKey } from "nostr-tools"
import { Session } from "../src/Session"
import { EXPIRATION_TAG } from "../src/types"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"

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
})

