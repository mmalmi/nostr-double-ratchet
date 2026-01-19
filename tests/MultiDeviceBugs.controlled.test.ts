import { describe, it, expect } from "vitest"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * Tests designed to catch multi-device bugs.
 *
 * These tests intentionally probe edge cases that have historically
 * caused issues with multi-device synchronization.
 */
describe("Multi-Device Bug Hunting", () => {
  describe("Concurrent sends from multiple devices of same user", () => {
    /**
     * If Alice has 2 devices and both send to Bob at the "same time",
     * does Bob receive both messages correctly?
     */
    it("should handle both of Alice's devices sending to Bob simultaneously", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish sessions from both Alice devices
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "init-from-alice-1",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "init-from-alice-2",
            waitOn: "auto",
          },
          // Bob replies to establish bidirectional
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "ack",
            waitOn: "auto",
          },
          // Now both Alice devices send WITHOUT waiting (simulating simultaneous)
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "concurrent-from-alice-1",
            ref: "c1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "concurrent-from-alice-2",
            ref: "c2",
          },
          // Deliver in interleaved order
          { type: "deliverEvent", ref: "c2" },
          { type: "deliverEvent", ref: "c1" },
          // Bob should receive BOTH
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "concurrent-from-alice-1" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "concurrent-from-alice-2" },
        ],
      })
    })

    it("should handle rapid alternating sends from Alice's two devices", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Establish sessions
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
          // Rapid alternating sends
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a1-msg1",
            ref: "m1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "a2-msg1",
            ref: "m2",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a1-msg2",
            ref: "m3",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "a2-msg2",
            ref: "m4",
          },
          // Deliver in scrambled order
          { type: "deliverInOrder", refs: ["m3", "m1", "m4", "m2"] },
          // All should be received
          {
            type: "expectAll",
            actor: "bob",
            deviceId: "bob-1",
            messages: ["a1-msg1", "a2-msg1", "a1-msg2", "a2-msg2"],
          },
        ],
      })
    })
  })

  describe("Different delivery order to different devices", () => {
    /**
     * Bob has 2 devices. Messages arrive in different order to each.
     * Both should eventually have all messages.
     */
    it("should handle messages delivered to Bob's devices in opposite orders", async () => {
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
          // Alice sends 3 messages
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
          // Deliver to bob-1 in order: 1, 2, 3
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "m1" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "m2" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "m3" },
          // Deliver to bob-2 in REVERSE order: 3, 2, 1
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "m3" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "m2" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "m1" },
          // Both devices should have all messages
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["msg-1", "msg-2", "msg-3"] },
          { type: "expectAll", actor: "bob", deviceId: "bob-2", messages: ["msg-1", "msg-2", "msg-3"] },
        ],
      })
    })

    it("should handle partial delivery to one device then full delivery to other", async () => {
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
          // Alice sends 3 messages
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
          // bob-1 only gets message 1
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "m1" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "msg-1" },
          // bob-2 gets all 3
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "m1" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "m2" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-2", ref: "m3" },
          { type: "expectAll", actor: "bob", deviceId: "bob-2", messages: ["msg-1", "msg-2", "msg-3"] },
          // Now bob-1 gets messages 2 and 3 (delayed)
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "m2" },
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "m3" },
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["msg-1", "msg-2", "msg-3"] },
        ],
      })
    })
  })

  describe("New device joining mid-conversation", () => {
    /**
     * A new device joins after significant message history.
     * Can it properly participate in the conversation?
     */
    it("should allow new device to send/receive after joining mid-conversation", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Extensive conversation
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-1",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "reply-1",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-2",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "reply-2",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "msg-3",
            waitOn: "auto",
          },
          // NOW Alice adds a second device
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          // New device should be able to send
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "from-new-device",
            waitOn: "auto",
          },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "from-new-device" },
          // And receive
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "to-new-device",
            waitOn: "auto",
          },
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "to-new-device" },
        ],
      })
    })

    it("should sync new device with messages sent before it joined", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Send several messages
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "before-join-1",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "before-join-2",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "before-join-3",
            waitOn: "auto",
          },
          // Alice adds new device - should it see old messages?
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          // The new device should see messages that were sent TO alice
          // (not necessarily the ones alice-1 sent)
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "before-join-2" },
        ],
      })
    })
  })

  describe("Cross-device session state consistency", () => {
    /**
     * When alice-1 sends and bob replies, does alice-2 have consistent state?
     */
    it("should maintain consistent state when only one device is active", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          // Only alice-1 communicates initially
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "from-alice-1-only",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "reply-to-alice",
            waitOn: "auto",
          },
          // alice-2 should have received the reply
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "reply-to-alice" },
          // Now alice-2 tries to send - does it work?
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "from-alice-2-after-alice-1-conversation",
            waitOn: "auto",
          },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "from-alice-2-after-alice-1-conversation" },
        ],
      })
    })

    it("should handle device-specific session rotation correctly", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-2" },
          // Complex multi-device conversation
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "a1-init",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "b1-reply",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "a2-message",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-2" },
            to: "alice",
            message: "b2-reply",
            waitOn: "auto",
          },
          // All devices should have received appropriate messages
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "a2-message" },
          { type: "expect", actor: "bob", deviceId: "bob-2", message: "a1-init" },
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "b2-reply" },
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "b1-reply" },
        ],
      })
    })
  })

  describe("Sender device copy synchronization", () => {
    /**
     * When alice-1 sends to bob, alice-2 should also get a copy.
     * This tests the "sender copy" functionality.
     */
    it("should deliver sender copies to other devices of the sender", async () => {
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
          // alice-1 sends to bob
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "alice-1-to-bob",
            waitOn: "auto",
          },
          // alice-2 should get a copy of what alice-1 sent
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "alice-1-to-bob" },
          // Same for alice-2 sending
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "alice-2-to-bob",
            waitOn: "auto",
          },
          // alice-1 should get a copy
          { type: "expect", actor: "alice", deviceId: "alice-1", message: "alice-2-to-bob" },
        ],
      })
    })

    it("should handle sender copies with delayed delivery", async () => {
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
          // alice-1 sends but we control delivery
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "delayed-copy-test",
            ref: "msg",
          },
          // Deliver to bob first
          { type: "deliverTo", actor: "bob", deviceId: "bob-1", ref: "msg" },
          { type: "expect", actor: "bob", deviceId: "bob-1", message: "delayed-copy-test" },
          // Then deliver copy to alice-2 (delayed)
          { type: "deliverTo", actor: "alice", deviceId: "alice-2", ref: "msg" },
          { type: "expect", actor: "alice", deviceId: "alice-2", message: "delayed-copy-test" },
        ],
      })
    })
  })

  describe("High contention scenarios", () => {
    /**
     * Many devices, many messages, complex ordering.
     */
    it("should handle 4 devices (2 per user) all messaging", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-2" },
          // Establish initial session
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
          // All 4 devices send messages
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-1" },
            to: "bob",
            message: "from-a1",
            ref: "a1",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-2" },
            to: "bob",
            message: "from-a2",
            ref: "a2",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-1" },
            to: "alice",
            message: "from-b1",
            ref: "b1",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-2" },
            to: "alice",
            message: "from-b2",
            ref: "b2",
          },
          // Deliver in scrambled order
          { type: "deliverInOrder", refs: ["b2", "a1", "b1", "a2"] },
          // All Bob's devices should have Alice's messages
          { type: "expectAll", actor: "bob", deviceId: "bob-1", messages: ["from-a1", "from-a2"] },
          { type: "expectAll", actor: "bob", deviceId: "bob-2", messages: ["from-a1", "from-a2"] },
          // All Alice's devices should have Bob's messages
          { type: "expectAll", actor: "alice", deviceId: "alice-1", messages: ["from-b1", "from-b2"] },
          { type: "expectAll", actor: "alice", deviceId: "alice-2", messages: ["from-b1", "from-b2"] },
        ],
      })
    })
  })
})
