import { describe, expect, it } from "vitest"
import {
  finalizeEvent,
  generateSecretKey,
  getPublicKey,
  type UnsignedEvent,
  type VerifiedEvent,
} from "nostr-tools"
import { SessionManager } from "../src/SessionManager"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { generateEphemeralKeypair, generateSharedSecret } from "../src/inviteUtils"
import { CHAT_MESSAGE_KIND, MESSAGE_EVENT_KIND, type Rumor } from "../src/types"
import type { UserRecordActor } from "../src/session-manager/UserRecordActor"

describe("SessionManager outer event metadata", () => {
  it("forwards the outer wrapper id through user-record device rumors", async () => {
    const ownerSecret = generateSecretKey()
    const ownerPubkey = getPublicKey(ownerSecret)
    const deviceSecret = generateSecretKey()
    const devicePubkey = getPublicKey(deviceSecret)
    const peerOwnerSecret = generateSecretKey()
    const peerOwnerPubkey = getPublicKey(peerOwnerSecret)
    const peerDevicePubkey = peerOwnerPubkey

    const manager = new SessionManager(
      devicePubkey,
      deviceSecret,
      devicePubkey,
      () => () => {},
      async (event: UnsignedEvent | VerifiedEvent) => event as VerifiedEvent,
      ownerPubkey,
      {
        ephemeralKeypair: generateEphemeralKeypair(),
        sharedSecret: generateSharedSecret(),
      },
      new InMemoryStorageAdapter(),
    )

    const received = new Promise<{ from: string; outerEventId?: string }>((resolve) => {
      manager.onEvent((_rumor, from, meta) => {
        resolve({ from, outerEventId: meta?.outerEventId })
      })
    })

    const userRecord = (
      manager as unknown as {
        getOrCreateUserRecord(pubkey: string): UserRecordActor
      }
    ).getOrCreateUserRecord(peerOwnerPubkey)

    const rumor: Rumor = {
      id: "inner-event-id",
      pubkey: peerDevicePubkey,
      kind: CHAT_MESSAGE_KIND,
      content: "hello",
      created_at: 1,
      tags: [],
    }
    const outerEvent = finalizeEvent(
      {
        kind: MESSAGE_EVENT_KIND,
        created_at: 1,
        content: "encrypted",
        tags: [],
      },
      peerOwnerSecret,
    ) as VerifiedEvent

    userRecord.onDeviceRumor(peerDevicePubkey, rumor, outerEvent)

    await expect(received).resolves.toEqual({
      from: peerOwnerPubkey,
      outerEventId: outerEvent.id,
    })
  })
})
