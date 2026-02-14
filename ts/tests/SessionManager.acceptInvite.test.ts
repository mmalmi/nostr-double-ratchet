import { describe, expect, it } from "vitest"
import type { VerifiedEvent } from "nostr-tools"
import { APP_KEYS_EVENT_KIND } from "../src/types"
import { AppKeys } from "../src/AppKeys"
import { Invite } from "../src/Invite"
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
})
