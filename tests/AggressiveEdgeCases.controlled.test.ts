import { describe, it, expect } from "vitest"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * More aggressive edge case tests designed to catch subtle bugs.
 *
 * These tests target specific race conditions and timing issues
 * that might not be caught by more straightforward tests.
 */
describe("Aggressive Edge Cases", () => {
  describe("Session establishment races", () => {
    /**
     * Both alice-1 and alice-2 try to establish a session with bob
     * at the exact same time (before seeing each other's invites).
     */
    it("should handle two devices racing to establish session with same recipient", async () => {
      const relay = new ControlledMockRelay()

      // Create all managers but DON'T auto-deliver during setup
      const { manager: alice1 } = await createControlledMockSessionManager(
        "alice-1",
        relay
      )
      const { manager: alice2, secretKey: aliceSecret } = await createControlledMockSessionManager(
        "alice-2",
        relay,
        undefined, // will get alice1's secret below
      )
      const { manager: bob, publicKey: bobPubkey } = await createControlledMockSessionManager(
        "bob-1",
        relay
      )

      const bobReceived: string[] = []
      bob.onEvent((e) => bobReceived.push(e.content))

      // Both Alice devices send to Bob "simultaneously"
      // (before delivery of session establishment)
      await alice1.sendMessage(bobPubkey, "from-alice-1")
      await alice2.sendMessage(bobPubkey, "from-alice-2")

      // Wait for async processing
      await new Promise((r) => setTimeout(r, 200))

      // Both messages should eventually be received
      expect(bobReceived).toContain("from-alice-1")
      expect(bobReceived).toContain("from-alice-2")
    })

    /**
     * Alice and Bob both try to initiate contact at the same time.
     */
    it("should handle mutual simultaneous session initiation", async () => {
      const relay = new ControlledMockRelay()

      const { manager: alice, publicKey: alicePubkey } =
        await createControlledMockSessionManager("alice-1", relay)
      const { manager: bob, publicKey: bobPubkey } =
        await createControlledMockSessionManager("bob-1", relay)

      const aliceReceived: string[] = []
      const bobReceived: string[] = []

      alice.onEvent((e) => aliceReceived.push(e.content))
      bob.onEvent((e) => bobReceived.push(e.content))

      // Both send at the "same time"
      await Promise.all([
        alice.sendMessage(bobPubkey, "alice-initiates"),
        bob.sendMessage(alicePubkey, "bob-initiates"),
      ])

      // Wait for async processing
      await new Promise((r) => setTimeout(r, 200))

      // Both should receive
      expect(bobReceived).toContain("alice-initiates")
      expect(aliceReceived).toContain("bob-initiates")
    })
  })

  describe("Self-messaging edge cases", () => {
    /**
     * BUG: Self-messaging doesn't deliver to sibling devices.
     * See BugReport.SelfMessage.test.ts for details.
     * Skipping until bug is fixed.
     */
    it.skip("BUG: should deliver self-messages to all sender devices", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "alice", deviceId: "alice-3" },
          // alice-1 sends to alice (self)
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "alice",
            message: "note-to-self",
            waitOn: "auto",
          },
          // All devices should receive it
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "note-to-self" },
          { type: "expect", actor: "alice", deviceId: "alice-3", message: "note-to-self" },
        ],
      })
    })

    it("should handle rapid self-messaging from different devices", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          // Both devices send self-messages
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "alice",
            message: "from-device-1",
            ref: "m1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "alice",
            message: "from-device-2",
            ref: "m2",
          },
          // Deliver in reverse order
          { type: "deliverEvent", ref: "m2" },
          { type: "deliverEvent", ref: "m1" },
          // Both devices should have both messages
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "from-device-2" },
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "from-device-1" },
        ],
      })
    })
  })

  describe("Large message gaps", () => {
    /**
     * Test the skipped keys limit - what happens with many skipped messages?
     */
    it("should handle 20 consecutive skipped messages", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish session
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // Send 21 messages
          ...Array.from({ length: 21 }, (_, i) => ({
            type: "send" as const,
            from: { actor: "alice" as const, deviceId: "alice-1" },
            to: "bob" as const,
            message: `msg-${i}`,
            ref: `m${i}`,
          })),
          // Deliver only the LAST one first (skipping 20)
          { type: "deliverEvent", ref: "m20" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-20" },
          // Now deliver the rest in order
          ...Array.from({ length: 20 }, (_, i) => ({
            type: "deliverEvent" as const,
            ref: `m${i}`,
          })),
          // All should be received
          {
            type: "expectAll",
            actor: "bob",
            deviceId: "bob-1",
            messages: Array.from({ length: 21 }, (_, i) => `msg-${i}`),
          },
        ],
      })
    })

    it("should handle gaps across ratchet rotations", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish session
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // Alice sends - delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "before-rotation-1",
            ref: "pre1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "before-rotation-2",
            ref: "pre2",
          },
          // Bob replies (rotates)
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "rotation-1",
            waitOn: "auto",
          },
          // Alice sends more - delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "after-rotation-1",
            ref: "post1",
          },
          // Bob replies again (rotates again)
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "rotation-2",
            waitOn: "auto",
          },
          // Alice sends more - delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "after-rotation-2",
            ref: "post2",
          },
          // Now deliver all delayed messages in REVERSE order
          { type: "deliverEvent", ref: "post2" },
          { type: "deliverEvent", ref: "post1" },
          { type: "deliverEvent", ref: "pre2" },
          { type: "deliverEvent", ref: "pre1" },
          // All should decrypt
          {
            type: "expectAll",
            actor: "bob",
            deviceId: "bob-1",
            messages: ["before-rotation-1", "before-rotation-2", "after-rotation-1", "after-rotation-2"],
          },
        ],
      })
    })
  })

  describe("Interleaved multi-party complexity", () => {
    /**
     * Complex interleaving of messages between multiple devices.
     */
    it("should handle complex 4-device interleaving with controlled delivery", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-2" },
          // Initial session establishment
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // Complex pattern: each device sends, controlled delivery order
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a1-1",
            ref: "a1_1",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "b1-1",
            ref: "b1_1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "a2-1",
            ref: "a2_1",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-2" },
            to: "alice",
            message: "b2-1",
            ref: "b2_1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a1-2",
            ref: "a1_2",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "b1-2",
            ref: "b1_2",
          },
          // Deliver to bob-1 in one order
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "a2_1" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "a1_2" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "a1_1" },
          // Deliver to bob-2 in different order
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "a1_1" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "a1_2" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "a2_1" },
          // Deliver to alice-1
          { type: "deliverTo", actor: "alice", deviceId: "alice-1", ref: "b2_1" },
          { type: "deliverTo", actor: "alice", deviceId: "alice-1", ref: "b1_1" },
          { type: "deliverTo", actor: "alice", deviceId: "alice-1", ref: "b1_2" },
          // Deliver to alice-2 in different order
          { type: "deliverTo", actor: "alice", deviceId: "alice-2", ref: "b1_2" },
          { type: "deliverTo", actor: "alice", deviceId: "alice-2", ref: "b2_1" },
          { type: "deliverTo", actor: "alice", deviceId: "alice-2", ref: "b1_1" },
          // Everyone should have everything
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["a1-1", "a2-1", "a1-2"] },
          { type: "expectAll", actor: "bob", deviceId: "bob-2", messages: ["a1-1", "a2-1", "a1-2"] },
          { type: "expectAll", actor: "alice", deviceId: "alice-1", messages: ["b1-1", "b2-1", "b1-2"] },
          { type: "expectAll", actor: "alice", deviceId: "alice-2", messages: ["b1-1", "b2-1", "b1-2"] },
        ],
      })
    })
  })

  describe("Device joins during active conversation", () => {
    /**
     * A third device joins while messages are in-flight.
     */
    it("should handle new device joining with pending undelivered messages", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish session
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // Bob sends a message - not delivered yet
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "before-new-device",
            ref: "pending",
          },
          // Alice adds a new device WHILE message is pending
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          // Now deliver the pending message
          { type: "deliverAll" },
          // Both Alice devices should get it
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "before-new-device" },
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "before-new-device" },
        ],
      })
    })
  })

  describe("Rapid device switching", () => {
    /**
     * User rapidly switches between devices while messaging.
     */
    it("should handle rapid device switching mid-conversation", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish session
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // Alice rapidly switches devices for each message
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "from-1a",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "from-2a",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "from-1b",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "from-2b",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "from-1c",
            waitOn: "auto",
          },
          // Bob should have all messages
          {
            type: "expectAll",
            actor: "bob",
            deviceId: "bob-1",
            messages: ["from-1a", "from-2a", "from-1b", "from-2b", "from-1c"],
          },
        ],
      })
    })
  })

  describe("Message delivery after extended offline period", () => {
    /**
     * Device goes offline for a while, many messages accumulate.
     */
    it("should handle device coming online after many messages accumulated", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish session
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // alice-2 goes offline
          { type: "close", actor: "alice", deviceId: "alice-2" },
          // Lots of messages while alice-2 is offline
          ...Array.from({ length: 10 }, (_, i) => ({
            type: "send" as const,
            from: { actor: "bob" as const, deviceId: "bob-1" },
            to: "alice" as const,
            message: `offline-msg-${i}`,
            waitOn: { actor: "alice" as const, deviceId: "alice-1" },
          })),
          // alice-2 comes back
          { type: "restart", actor: "alice", deviceId: "alice-2" },
          // alice-2 should receive all the messages
          {
            type: "expectAll",
            actor: "alice",
            deviceId: "alice-2",
            messages: Array.from({ length: 10 }, (_, i) => `offline-msg-${i}`),
          },
        ],
      })
    })
  })
})
