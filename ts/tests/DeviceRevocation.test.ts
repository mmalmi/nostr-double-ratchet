import { describe, it, expect } from "vitest"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { runScenario } from "./helpers/scenario"
import { Rumor } from "../src/types"

/**
 * Poll a predicate every 50ms until it returns true or timeout expires.
 */
async function waitForCondition(
  predicate: () => boolean,
  timeoutMs = 5000,
  label = "condition",
): Promise<void> {
  const start = Date.now()
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return
    await new Promise((r) => setTimeout(r, 50))
  }
  throw new Error(`Timed out waiting for ${label} after ${timeoutMs}ms`)
}

/**
 * Create Alice (2 devices) + Bob (1 device) with established bidirectional sessions.
 * Returns all actors, message tracking arrays, and the shared relay.
 */
async function setupMultiDeviceWithSessions() {
  const relay = new MockRelay()

  // Alice device 1
  const alice1 = await createMockSessionManager("alice-d1", relay)

  // Alice device 2 — reuses Alice's owner key so both devices share identity
  const alice2 = await createMockSessionManager("alice-d2", relay, alice1.secretKey)

  // Bob device 1
  const bob = await createMockSessionManager("bob-d1", relay)

  // Track received messages per device
  const alice1Messages: string[] = []
  const alice2Messages: string[] = []
  const bobMessages: string[] = []

  alice1.manager.onEvent((event: Rumor) => alice1Messages.push(event.content))
  alice2.manager.onEvent((event: Rumor) => alice2Messages.push(event.content))
  bob.manager.onEvent((event: Rumor) => bobMessages.push(event.content))

  // Bob sends to Alice → both devices receive (establishes bob→alice sessions)
  await bob.manager.sendMessage(alice1.publicKey, "setup-bob-to-alice")
  await waitForCondition(
    () => alice1Messages.includes("setup-bob-to-alice") && alice2Messages.includes("setup-bob-to-alice"),
    5000,
    "both alice devices receive bob's message",
  )

  // alice-d1 replies → bob receives (establishes alice-d1→bob session)
  await alice1.manager.sendMessage(bob.publicKey, "setup-alice1-to-bob")
  await waitForCondition(
    () => bobMessages.includes("setup-alice1-to-bob"),
    5000,
    "bob receives alice1 reply",
  )

  // alice-d2 replies → bob receives (establishes alice-d2→bob session)
  await alice2.manager.sendMessage(bob.publicKey, "setup-alice2-to-bob")
  await waitForCondition(
    () => bobMessages.includes("setup-alice2-to-bob"),
    5000,
    "bob receives alice2 reply",
  )

  // Clear tracking arrays so tests start clean
  alice1Messages.length = 0
  alice2Messages.length = 0
  bobMessages.length = 0

  return { relay, alice1, alice2, bob, alice1Messages, alice2Messages, bobMessages }
}

/**
 * Revoke alice-d2 and wait for propagation.
 * Uses alice2.appKeysManager because it was created second and has both devices registered.
 */
async function revokeAliceDevice2(alice2: Awaited<ReturnType<typeof createMockSessionManager>>) {
  alice2.appKeysManager.revokeDevice(alice2.manager.getDeviceId())
  await alice2.appKeysManager.publish()
  // Allow propagation — relay delivers synchronously but AppKeys processing triggers async work
  await new Promise((r) => setTimeout(r, 200))
}

describe("Device Revocation Enforcement", () => {
  it("should remove device record from peer after revocation", async () => {
    const { alice1, alice2, bob } = await setupMultiDeviceWithSessions()

    // Pre-assert: Bob has device records for both alice devices
    const bobRecords = bob.manager.getUserRecords()
    const aliceRecord = bobRecords.get(alice1.publicKey)
    expect(aliceRecord).toBeDefined()
    expect(aliceRecord!.devices.size).toBeGreaterThanOrEqual(2)

    const alice2DeviceId = alice2.manager.getDeviceId()
    expect(aliceRecord!.devices.has(alice2DeviceId)).toBe(true)

    // Revoke alice-d2
    await revokeAliceDevice2(alice2)

    // Bob's record for Alice should no longer have alice-d2
    await waitForCondition(() => {
      const records = bob.manager.getUserRecords()
      const rec = records.get(alice1.publicKey)
      return rec !== undefined && !rec.devices.has(alice2DeviceId)
    }, 5000, "bob removes alice-d2 device record")

    // alice-d1 should still be present
    const updatedRecord = bob.manager.getUserRecords().get(alice1.publicKey)!
    const alice1DeviceId = alice1.manager.getDeviceId()
    expect(updatedRecord.devices.has(alice1DeviceId)).toBe(true)
  }, 30000)

  it("should not send to revoked device", async () => {
    const { alice1, alice2, bob, alice1Messages, alice2Messages } = await setupMultiDeviceWithSessions()

    // Revoke alice-d2
    await revokeAliceDevice2(alice2)

    // Bob sends a message
    await bob.manager.sendMessage(alice1.publicKey, "after-revoke")

    // alice-d1 receives it
    await waitForCondition(
      () => alice1Messages.includes("after-revoke"),
      5000,
      "alice-d1 receives message",
    )

    // alice-d2 should NOT receive it — explicit non-delivery check
    await new Promise((r) => setTimeout(r, 300))
    expect(alice2Messages).not.toContain("after-revoke")
  }, 30000)

  it("should not deliver revoked device's messages to peer", async () => {
    const { alice2, bob, bobMessages } = await setupMultiDeviceWithSessions()

    // Revoke alice-d2
    await revokeAliceDevice2(alice2)

    // alice-d2 tries to send — its local session to Bob still exists,
    // but Bob's subscription for that device was removed by cleanupDevice
    await alice2.manager.sendMessage(bob.publicKey, "from-revoked-device")

    // Wait and assert Bob does NOT receive it
    await new Promise((r) => setTimeout(r, 300))
    expect(bobMessages).not.toContain("from-revoked-device")
  }, 30000)

  it("should continue communication after revocation (positive control)", async () => {
    const { alice1, alice2, bob, alice1Messages, bobMessages } = await setupMultiDeviceWithSessions()

    // Revoke alice-d2
    await revokeAliceDevice2(alice2)

    // Bob → alice-d1 still works
    await bob.manager.sendMessage(alice1.publicKey, "post-revoke-1")
    await waitForCondition(
      () => alice1Messages.includes("post-revoke-1"),
      5000,
      "alice-d1 receives post-revoke message from bob",
    )

    // alice-d1 → Bob still works
    await alice1.manager.sendMessage(bob.publicKey, "post-revoke-2")
    await waitForCondition(
      () => bobMessages.includes("post-revoke-2"),
      5000,
      "bob receives post-revoke message from alice-d1",
    )
  }, 30000)

  it("scenario runner: revocation stops delivery (positive control)", async () => {
    await runScenario({
      steps: [
        // Setup: Alice has 2 devices, Bob has 1 device
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDelegateDevice", actor: "alice", deviceId: "alice-device-2", mainDeviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },

        // Establish sessions between all devices
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "hello",
          waitOn: "all-recipient-devices" },
        { type: "expect", actor: "alice", deviceId: "alice-device-1", message: "hello" },
        { type: "expect", actor: "alice", deviceId: "alice-device-2", message: "hello" },

        // Both alice devices reply
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "reply-1" },
        { type: "expect", actor: "bob", deviceId: "bob-device-1", message: "reply-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-2" }, to: "bob", message: "reply-2" },
        { type: "expect", actor: "bob", deviceId: "bob-device-1", message: "reply-2" },

        // Revoke alice-device-2
        { type: "removeDevice", actor: "alice", deviceId: "alice-device-2" },

        // Bob sends — only alice-device-1 should receive
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "after-revoke",
          waitOn: { actor: "alice", deviceId: "alice-device-1" } },
        { type: "expect", actor: "alice", deviceId: "alice-device-1", message: "after-revoke" },

        // alice-device-1 ↔ bob bidirectional still works
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "still-works" },
        { type: "expect", actor: "bob", deviceId: "bob-device-1", message: "still-works" },
      ],
    })
  }, 30000)
})
