import { describe, expect, it } from "vitest"
import { generateSecretKey, getPublicKey, type VerifiedEvent } from "nostr-tools"
import { Session } from "../src/Session"
import { decryptSessionEventPreview } from "../src/notificationPreview"
import { deserializeSessionState, serializeSessionState } from "../src/utils"

const sharedSecret = () => new Uint8Array()

function createPair() {
  const aliceSecretKey = generateSecretKey()
  const bobSecretKey = generateSecretKey()

  const alice = Session.init(
    getPublicKey(bobSecretKey),
    aliceSecretKey,
    true,
    sharedSecret(),
    "alice",
  )
  const bob = Session.init(
    alice.state.ourCurrentNostrKey!.publicKey,
    bobSecretKey,
    false,
    sharedSecret(),
    "bob",
  )

  return { alice, bob }
}

describe("decryptSessionEventPreview", () => {
  it("decrypts with the matching candidate without mutating durable state", () => {
    const { alice, bob } = createPair()
    const outerEvent = alice.send("push preview").event as VerifiedEvent
    const originalSerializedState = serializeSessionState(bob.state)

    const preview = decryptSessionEventPreview(outerEvent, [
      { state: originalSerializedState, chatId: "alice", context: { source: "stored" } },
    ])

    expect(preview?.rumor.content).toBe("push preview")
    expect(preview?.chatId).toBe("alice")
    expect(preview?.candidateIndex).toBe(0)
    expect(preview?.context).toEqual({ source: "stored" })
    expect(serializeSessionState(bob.state)).toBe(originalSerializedState)
  })

  it("skips non-matching candidates and leaves object states untouched", () => {
    const { alice, bob } = createPair()
    const otherPair = createPair()
    const outerEvent = alice.send("for bob").event as VerifiedEvent
    const bobStateBefore = serializeSessionState(bob.state)
    const otherStateBefore = serializeSessionState(otherPair.bob.state)

    const preview = decryptSessionEventPreview(outerEvent, [
      { state: otherPair.bob.state, chatId: "other" },
      { state: deserializeSessionState(bobStateBefore), chatId: "bob" },
    ])

    expect(preview?.rumor.content).toBe("for bob")
    expect(preview?.chatId).toBe("bob")
    expect(preview?.candidateIndex).toBe(1)
    expect(serializeSessionState(otherPair.bob.state)).toBe(otherStateBefore)
    expect(serializeSessionState(bob.state)).toBe(bobStateBefore)
  })
})
