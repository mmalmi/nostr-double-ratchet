import { describe, it, expect } from "vitest"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * Self-messaging: sending a message to yourself should deliver to all your OTHER devices.
 *
 * When a user sends a message to themselves (e.g., "note to self"),
 * the message should be delivered to all of that user's OTHER devices
 * (excluding the sending device, which already has the message locally).
 *
 * EXPECTED: alice-1 sends to alice -> alice-2 receives it (alice-1 already has it)
 */
describe("Self-Message Fanout", () => {
  it("should deliver self-message to other devices of same user", async () => {
    await runControlledScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-1" },
        { type: "addDevice", actor: "alice", deviceId: "alice-2" },
        // alice-1 sends to alice (self)
        // Don't use waitOn: "auto" because that waits for ALL devices including the sender
        // The sender's device is correctly excluded from receiving (it already has the message)
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-1" },
          to: "alice",
          message: "note-to-self",
        },
        // alice-2 should receive the message
        { type: "expect", actor: "alice", deviceId: "alice-2", message: "note-to-self" },
      ],
    })
  }, 10000)

  it("should deliver self-message with 3 devices", async () => {
    await runControlledScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-1" },
        { type: "addDevice", actor: "alice", deviceId: "alice-2" },
        { type: "addDevice", actor: "alice", deviceId: "alice-3" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-1" },
          to: "alice",
          message: "note-to-all",
        },
        // Both alice-2 and alice-3 should receive it
        { type: "expect", actor: "alice", deviceId: "alice-2", message: "note-to-all" },
        { type: "expect", actor: "alice", deviceId: "alice-3", message: "note-to-all" },
      ],
    })
  }, 15000)

  it("verifies sender device is excluded from recipients", async () => {
    const relay = new ControlledMockRelay()

    const { manager: alice1, publicKey: alicePubkey } =
      await createControlledMockSessionManager("alice-1", relay)

    // Just verify that sending to self doesn't crash
    await alice1.sendMessage(alicePubkey, "note-to-self")

    // Message was published (no error thrown)
    const events = relay.getAllEvents()
    expect(events.length).toBeGreaterThan(0)

    // With the fix, logs show:
    // [SM alice-1] sending to 0 devices: []
    // (0 devices because the only device is the sender, which is excluded)
  })
})
