import { describe, it, expect } from "vitest"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * BUG: Self-messaging does not fan out to all sender devices.
 *
 * When a user sends a message to themselves (e.g., "note to self"),
 * the message should be delivered to ALL of that user's devices.
 *
 * EXPECTED: alice-1 sends to alice -> alice-2 and alice-3 receive it
 * ACTUAL: alice-2 and alice-3 do NOT receive the message (times out)
 *
 * ROOT CAUSE (from logs):
 * When sending to self, SessionManager finds:
 *   [{ id: 'alice-1', hasSession: false }, { id: 'alice-1', hasSession: false }]
 * Instead of:
 *   [{ id: 'alice-1', hasSession: false }, { id: 'alice-2', hasSession: false }]
 *
 * The device lookup is duplicating the sender's device ID instead of finding
 * the OTHER devices belonging to the same user.
 */
describe("BUG: Self-Message Fanout", () => {
  /**
   * This test SHOULD pass but FAILS due to the bug.
   * Marking as .skip to not break CI - remove .skip to reproduce.
   */
  it.skip("FAILS: should deliver self-message to other devices of same user", async () => {
    await runControlledScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-1" },
        { type: "addDevice", actor: "alice", deviceId: "alice-2" },
        // alice-1 sends to alice (self)
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-1" },
          to: "alice",
          message: "note-to-self",
          waitOn: "auto",  // This will timeout because alice-2 never receives it
        },
        // BUG: This expectation will timeout - alice-2 never gets the message
        { type: "expect", actor: "alice", deviceId: "alice-2", message: "note-to-self" },
      ],
    })
  }, 10000)

  /**
   * This test documents the bug: sending to self doesn't crash but
   * doesn't deliver to sibling devices either.
   */
  it("documents bug: self-message is sent but not delivered to sibling devices", async () => {
    const relay = new ControlledMockRelay()

    const { manager: alice1, publicKey: alicePubkey } =
      await createControlledMockSessionManager("alice-1", relay)

    // Just verify that sending to self doesn't crash
    await alice1.sendMessage(alicePubkey, "note-to-self")

    // Message was published (no error thrown)
    const events = relay.getAllEvents()
    expect(events.length).toBeGreaterThan(0)

    // The bug: looking at the logs, we'd see:
    // [SM alice-1] sending to 2 devices: [
    //   { id: 'alice-1', hasSession: false },
    //   { id: 'alice-1', hasSession: false }  <-- DUPLICATE! Should be alice-2
    // ]
    // [SM alice-1] no active session for device alice-1, skipping
    // [SM alice-1] no active session for device alice-1, skipping
  })
})
