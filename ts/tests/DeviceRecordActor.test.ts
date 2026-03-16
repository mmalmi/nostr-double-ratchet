import { describe, expect, it, vi } from "vitest"
import { finalizeEvent, generateSecretKey, getPublicKey, type UnsignedEvent, type VerifiedEvent } from "nostr-tools"
import { Invite } from "../src/Invite"
import { MessageQueue } from "../src/MessageQueue"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { DeviceRecordActor } from "../src/session-manager/DeviceRecordActor"
import { createSessionFromAccept, decryptInviteResponse } from "../src/inviteUtils"
import type { NostrFacade, DeviceRecordUserHooks } from "../src/session-manager/types"
import type { NostrSubscribe, Rumor } from "../src/types"
import { MockRelay } from "./helpers/mockRelay"

function createSubscribe(relay: MockRelay): NostrSubscribe {
  return (filter, onEvent) => {
    const handle = relay.subscribe(filter, onEvent)
    return handle.close
  }
}

async function waitForSessionMessage(
  session: { onEvent: (cb: (event: Rumor) => void) => () => void },
  text: string,
  timeoutMs = 10_000,
): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => {
      unsubscribe()
      reject(new Error(`Timed out waiting for session message "${text}"`))
    }, timeoutMs)

    const unsubscribe = session.onEvent((event) => {
      if (event.content != text) {
        return
      }
      clearTimeout(timeout)
      unsubscribe()
      resolve()
    })
  })
}

describe("DeviceRecordActor", () => {
  it("flushes queued messages after the first inbound event makes a passive session sendable", async () => {
    const relay = new MockRelay()
    const passiveSideSecretKey = generateSecretKey()
    const passiveSidePublicKey = getPublicKey(passiveSideSecretKey)
    const activeSideSecretKey = generateSecretKey()
    const activeSidePublicKey = getPublicKey(activeSideSecretKey)
    const subscribe = createSubscribe(relay)

    const invite = Invite.createNew(passiveSidePublicKey, passiveSidePublicKey, 1, {
      purpose: "link",
      ownerPubkey: passiveSidePublicKey,
    })

    const { session: activeSession, event: responseEvent } = await invite.accept(
      subscribe,
      activeSidePublicKey,
      activeSideSecretKey,
      activeSidePublicKey,
    )

    const decrypted = await decryptInviteResponse({
      envelopeContent: responseEvent.content,
      envelopeSenderPubkey: responseEvent.pubkey,
      inviterEphemeralPrivateKey: invite.inviterEphemeralPrivateKey!,
      inviterPrivateKey: passiveSideSecretKey,
      sharedSecret: invite.sharedSecret,
    })

    const passiveSession = createSessionFromAccept({
      nostrSubscribe: subscribe,
      theirPublicKey: decrypted.inviteeSessionPublicKey,
      ourSessionPrivateKey: invite.inviterEphemeralPrivateKey!,
      sharedSecret: invite.sharedSecret,
      isSender: false,
      name: responseEvent.id,
    })

    const messageQueue = new MessageQueue(
      new InMemoryStorageAdapter(),
      "v1/device-record-test/",
    )

    const userHooks: DeviceRecordUserHooks = {
      isDeviceAuthorized: vi.fn(() => true),
      onDeviceRumor: vi.fn(),
      onDeviceDirty: vi.fn(),
    }

    const nostr: NostrFacade = {
      subscribe,
      publish: vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
        relay.storeAndDeliver(event as VerifiedEvent)
      }),
    }

    const actor = new DeviceRecordActor(passiveSidePublicKey, {
      ownerPubkey: passiveSidePublicKey,
      user: userHooks,
      nostr,
      messageQueue,
      ourDeviceId: activeSidePublicKey,
      ourOwnerPubkey: activeSidePublicKey,
      identityKey: activeSideSecretKey,
    })

    actor.installSession(passiveSession)

    const queuedText = `queued-after-bootstrap-${Date.now()}`
    const queuedRumor: Rumor = {
      id: `rumor-${Date.now()}`,
      pubkey: activeSidePublicKey,
      kind: 14,
      content: queuedText,
      created_at: Math.floor(Date.now() / 1000),
      tags: [["p", passiveSidePublicKey]],
    }
    await messageQueue.add(passiveSidePublicKey, queuedRumor)

    const before = await messageQueue.getForTarget(passiveSidePublicKey)
    expect(before).toHaveLength(1)

    const receivedQueuedMessage = waitForSessionMessage(activeSession, queuedText)

    const { event: bootstrapEvent } = activeSession.sendTyping({
      expiresAt: Math.floor(Date.now() / 1000),
    })
    relay.storeAndDeliver(bootstrapEvent)

    await receivedQueuedMessage

    const after = await messageQueue.getForTarget(passiveSidePublicKey)
    expect(after).toHaveLength(0)
    expect(nostr.publish).toHaveBeenCalled()
  })
})
