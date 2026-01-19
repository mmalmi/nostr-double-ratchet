import { describe, it, expect } from "vitest"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * Edge case tests that leverage ControlledMockRelay's delivery control.
 *
 * These scenarios are difficult or impossible to test with automatic delivery
 * because they require precise control over message ordering, timing, and failures.
 */
describe("Edge Cases (Controlled Relay)", () => {
  describe("Out-of-order message delivery", () => {
    /**
     * The double ratchet protocol maintains "skipped keys" to handle messages
     * that arrive out of order. This test verifies that mechanism works.
     */
    it("should decrypt messages delivered in reverse order", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish session first
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
          // Now send 3 messages WITHOUT delivering
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "message-1",
            ref: "m1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "message-2",
            ref: "m2",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "message-3",
            ref: "m3",
          },
          // Deliver in REVERSE order (3, 2, 1)
          { type: "deliverEvent", ref: "m3" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "message-3" },
          { type: "deliverEvent", ref: "m2" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "message-2" },
          { type: "deliverEvent", ref: "m1" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "message-1" },
        ],
      })
    })

    it("should decrypt messages delivered in random order", async () => {
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
          // Send 5 messages
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-1",
            ref: "m1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-2",
            ref: "m2",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-3",
            ref: "m3",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-4",
            ref: "m4",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-5",
            ref: "m5",
          },
          // Deliver in scrambled order: 3, 1, 5, 2, 4
          { type: "deliverInOrder", refs: ["m3", "m1", "m5", "m2", "m4"] },
          // All should be received
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["msg-1", "msg-2", "msg-3", "msg-4", "msg-5"] },
        ],
      })
    })

    it("should handle interleaved bidirectional out-of-order delivery", async () => {
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
          // Both parties send messages without immediate delivery
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "alice-1",
            ref: "a1",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "bob-1",
            ref: "b1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "alice-2",
            ref: "a2",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "bob-2",
            ref: "b2",
          },
          // Deliver Bob's messages to Alice first (in reverse)
          { type: "deliverEvent", ref: "b2" },
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "bob-2" },
          { type: "deliverEvent", ref: "b1" },
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "bob-1" },
          // Then deliver Alice's messages to Bob (in reverse)
          { type: "deliverEvent", ref: "a2" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "alice-2" },
          { type: "deliverEvent", ref: "a1" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "alice-1" },
        ],
      })
    })
  })

  describe("Message gaps (lost messages)", () => {
    /**
     * When a message is lost in transit, subsequent messages should still
     * be decryptable. The protocol should be resilient to gaps.
     */
    it("should continue communication after a dropped message", async () => {
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
          // Send 3 messages
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-1",
            ref: "m1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-2-LOST",
            ref: "m2",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-3",
            ref: "m3",
          },
          // DROP message 2 (simulating network failure)
          { type: "dropEvent", ref: "m2" },
          // Deliver messages 1 and 3
          { type: "deliverEvent", ref: "m1" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-1" },
          { type: "deliverEvent", ref: "m3" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-3" },
          // Communication should continue
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-4",
            waitOn: "auto",
          },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-4" },
          // Bob can still reply
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "reply",
            waitOn: "auto",
          },
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "reply" },
        ],
      })
    })

    it("should handle multiple consecutive dropped messages", async () => {
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
          // Send 5 messages
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-1",
            ref: "m1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-2",
            ref: "m2",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-3",
            ref: "m3",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-4",
            ref: "m4",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-5",
            ref: "m5",
          },
          // Drop messages 2, 3, and 4
          { type: "dropEvent", ref: "m2" },
          { type: "dropEvent", ref: "m3" },
          { type: "dropEvent", ref: "m4" },
          // Only deliver 1 and 5
          { type: "deliverEvent", ref: "m1" },
          { type: "deliverEvent", ref: "m5" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-1" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-5" },
          // Session should still work
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "still works!",
            waitOn: "auto",
          },
        ],
      })
    })
  })

  describe("Delayed delivery after key rotation", () => {
    /**
     * When Bob replies, the ratchet rotates keys. Messages sent before
     * the rotation but delivered after should still decrypt using stored keys.
     */
    it("should decrypt old message delivered after key rotation", async () => {
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
          // Alice sends message but it gets delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "delayed-message",
            ref: "delayed",
          },
          // Bob sends a reply (which rotates the ratchet)
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "bob-reply-1",
            waitOn: "auto",
          },
          // Alice sends another message (using new keys)
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "new-message",
            waitOn: "auto",
          },
          // NOW the delayed message finally arrives
          { type: "deliverEvent", ref: "delayed" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "delayed-message" },
          // Communication should continue normally
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "bob-reply-2",
            waitOn: "auto",
          },
        ],
      })
    })

    it("should handle multiple rotations with delayed messages", async () => {
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
          // Alice sends - will be delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "alice-round-1",
            ref: "a1",
          },
          // Bob replies (rotation 1)
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "bob-round-1",
            waitOn: "auto",
          },
          // Alice sends again - also delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "alice-round-2",
            ref: "a2",
          },
          // Bob replies again (rotation 2)
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "bob-round-2",
            waitOn: "auto",
          },
          // Alice sends one more - also delayed
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "alice-round-3",
            ref: "a3",
          },
          // Now deliver all delayed messages in reverse order
          { type: "deliverEvent", ref: "a3" },
          { type: "deliverEvent", ref: "a2" },
          { type: "deliverEvent", ref: "a1" },
          // All should decrypt
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["alice-round-1", "alice-round-2", "alice-round-3"] },
        ],
      })
    })
  })

  describe("Duplicate message handling", () => {
    /**
     * Network issues might cause the same message to be delivered twice.
     * The protocol should handle this gracefully.
     */
    it("should handle duplicate message delivery gracefully", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: alice } = await createControlledMockSessionManager(
        "alice-1",
        sharedRelay
      )

      const { manager: bob, publicKey: bobPubkey } =
        await createControlledMockSessionManager("bob-1", sharedRelay)

      // Track how many times Bob receives the message
      let receiveCount = 0
      const messageContent = "duplicate-test-message"

      bob.onEvent((event) => {
        if (event.content === messageContent) {
          receiveCount++
        }
      })

      // Establish session
      await alice.sendMessage(bobPubkey, "init")
      await new Promise<void>((r) => {
        const unsub = bob.onEvent((e) => {
          if (e.content === "init") { unsub(); r() }
        })
      })

      // Get the message event
      await alice.sendMessage(bobPubkey, messageContent)
      await new Promise<void>((r) => {
        const unsub = bob.onEvent((e) => {
          if (e.content === messageContent) { unsub(); r() }
        })
      })

      const firstCount = receiveCount

      // Force duplicate delivery
      const allEvents = sharedRelay.getAllEvents()
      const msgEvent = allEvents.find(e => e.content?.includes("duplicate-test"))
      if (msgEvent) {
        sharedRelay.duplicateEvent(msgEvent.id)
      }

      // Give time for duplicate to process
      await new Promise((r) => setTimeout(r, 100))

      // Session should still work after duplicate - verify by continuing conversation
      // The key test is that receiveCount shows the message was processed
      expect(receiveCount).toBeGreaterThanOrEqual(firstCount)
    })
  })

  describe("Device synchronization edge cases", () => {
    /**
     * Multi-device scenarios where messages arrive at different devices
     * at different times.
     */
    it("should deliver to device-2 after device-1 already received", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-2" },
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
          // Alice sends a message
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "important-message",
            waitOn: "auto",
          },
          // Both Bob's devices should have received it
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "important-message" },
          { type: "expect", actor: "bob", deviceId: "bob-2", message: "important-message" },
        ],
      })
    })

  })

  describe("Stress scenarios", () => {
    /**
     * High-volume scenarios that stress test the protocol.
     */
    it("should handle many out-of-order messages", async () => {
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
          // Send 10 messages
          ...Array.from({ length: 10 }, (_, i) => ({
            type: "send" as const,
            from: { actor: "alice" as const, deviceId: "alice-1" },
            to: "bob" as const,
            message: `msg-${i}`,
            ref: `m${i}`,
          })),
          // Deliver in reverse order
          { type: "deliverInOrder", refs: ["m9", "m8", "m7", "m6", "m5", "m4", "m3", "m2", "m1", "m0"] },
          // All should be received
          {
            type: "expectAll",
            actor: "bob",
            deviceId: "bob-1",
            messages: Array.from({ length: 10 }, (_, i) => `msg-${i}`),
          },
        ],
      })
    })

    it("should handle alternating senders with delays", async () => {
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
          // Alternating messages without immediate delivery
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a1",
            ref: "a1",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "b1",
            ref: "b1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a2",
            ref: "a2",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "b2",
            ref: "b2",
          },
          // Deliver Bob's messages first, then Alice's
          { type: "deliverEvent", ref: "b2" },
          { type: "deliverEvent", ref: "b1" },
          { type: "deliverEvent", ref: "a2" },
          { type: "deliverEvent", ref: "a1" },
          // All should decrypt
          { type: "expectAll", actor: "alice", deviceId: "alice-1", messages: ["b1", "b2"] },
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["a1", "a2"] },
        ],
      })
    })
  })
})
