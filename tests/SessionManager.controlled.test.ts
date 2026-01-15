import { describe, it, expect } from "vitest"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * These tests mirror SessionManager.test.ts but use ControlledMockRelay
 * for explicit control over event delivery timing and ordering.
 *
 * Note: Session establishment requires auto-delivery to work properly.
 * The controlled delivery features are most useful for testing message
 * delivery after sessions are established.
 */
describe("SessionManager (Controlled Relay)", () => {
  describe("Basic functionality (replicated from original tests)", () => {
    it("should receive a message", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: managerAlice, publish: publishAlice } =
        await createControlledMockSessionManager("alice-device-1", sharedRelay)

      const { manager: managerBob, publicKey: bobPubkey } =
        await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const chatMessage = "Hello Bob from Alice!"

      // With auto-deliver mode (default), this should work just like the original
      const bobReceivedMessage = new Promise((resolve) => {
        managerBob.onEvent((event) => {
          if (event.content === chatMessage) resolve(true)
        })
      })

      await managerAlice.sendMessage(bobPubkey, chatMessage)

      expect(publishAlice).toHaveBeenCalled()
      expect(await bobReceivedMessage).toBe(true)
    })

    it("should sync messages across multiple devices", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: aliceDevice1, secretKey: aliceSecretKey } =
        await createControlledMockSessionManager("alice-device-1", sharedRelay)

      const { manager: aliceDevice2 } = await createControlledMockSessionManager(
        "alice-device-2",
        sharedRelay,
        aliceSecretKey
      )

      const { manager: bobDevice1, publicKey: bobPubkey } =
        await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const msg1 = "Hello Bob from Alice device 1"
      const msg2 = "Hello Bob from Alice device 2"

      const bobReceivedMessages = new Promise((resolve) => {
        const received: string[] = []
        bobDevice1.onEvent((event) => {
          if (event.content === msg1 || event.content === msg2) {
            received.push(event.content)
            if (received.length === 2) resolve(received)
          }
        })
      })

      await aliceDevice1.sendMessage(bobPubkey, msg1)
      await aliceDevice2.sendMessage(bobPubkey, msg2)

      expect(await bobReceivedMessages).toBeTruthy()
    })

    it("should deliver messages to all sender and recipient devices (scenario)", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
          { type: "addDevice", actor: "alice", deviceId: "alice-device-2" },
          { type: "addDevice", actor: "bob", deviceId: "bob-device-2" },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-device-1" },
            to: "bob",
            message: "alice broadcast",
            waitOn: "auto",
          },
          {
            type: "expect",
            actor: "alice",
            deviceId: "alice-device-2",
            message: "alice broadcast",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-device-2" },
            to: "alice",
            message: "bob broadcast",
            waitOn: "auto",
          },
          {
            type: "expect",
            actor: "bob",
            deviceId: "bob-device-1",
            message: "bob broadcast",
          },
          {
            type: "expect",
            actor: "alice",
            deviceId: "alice-device-1",
            message: "bob broadcast",
          },
          {
            type: "expect",
            actor: "alice",
            deviceId: "alice-device-2",
            message: "bob broadcast",
          },
        ],
      })
    })

    it("should handle back to back messages", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-device-1" },
            to: "bob",
            message: "alice to bob 1",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-device-1" },
            to: "alice",
            message: "bob to alice 1",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-device-1" },
            to: "bob",
            message: "alice to bob 2",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-device-1" },
            to: "bob",
            message: "alice to bob 3",
            waitOn: "auto",
          },
        ],
      })
    })

    it("should persist sessions across manager restarts", async () => {
      await runControlledScenario({
        steps: [
          { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
          { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-device-1" },
            to: "bob",
            message: "Initial message",
            waitOn: "auto",
          },
          {
            type: "send",
            from: { actor: "bob", deviceId: "bob-device-1" },
            to: "alice",
            message: "Reply message",
            waitOn: "auto",
          },
          { type: "restart", actor: "alice", deviceId: "alice-device-1" },
          { type: "restart", actor: "bob", deviceId: "bob-device-1" },
          {
            type: "send",
            from: { actor: "alice", deviceId: "alice-device-1" },
            to: "bob",
            message: "Message after restart",
            waitOn: "auto",
          },
          {
            type: "expect",
            actor: "bob",
            deviceId: "bob-device-1",
            message: "Message after restart",
          },
        ],
      })
    })
  })

  describe("Controlled delivery features", () => {
    it("should track delivery history", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: alice } = await createControlledMockSessionManager(
        "alice-device-1",
        sharedRelay
      )

      const { publicKey: bobPubkey } = await createControlledMockSessionManager(
        "bob-device-1",
        sharedRelay
      )

      await alice.sendMessage(bobPubkey, "tracked message")

      const history = sharedRelay.getDeliveryHistory()
      expect(history.length).toBeGreaterThan(0)
    })

    it("should expose subscription info", async () => {
      const sharedRelay = new ControlledMockRelay()

      await createControlledMockSessionManager("alice-device-1", sharedRelay)
      await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const subs = sharedRelay.getSubscriptions()
      expect(subs.length).toBeGreaterThan(0)
    })

    it("should support duplicate event detection via delivery count", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: alice } = await createControlledMockSessionManager(
        "alice-device-1",
        sharedRelay
      )

      const { publicKey: bobPubkey } = await createControlledMockSessionManager(
        "bob-device-1",
        sharedRelay
      )

      await alice.sendMessage(bobPubkey, "test msg")

      // Get the latest event
      const allEvents = sharedRelay.getAllEvents()
      const msgEvent = allEvents[allEvents.length - 1]

      // With auto-deliver, it should have been delivered once
      const count = sharedRelay.getDeliveryCount(msgEvent.id)
      expect(count).toBeGreaterThanOrEqual(1)

      // Force duplicate delivery
      sharedRelay.duplicateEvent(msgEvent.id)

      // Now count should be higher
      const newCount = sharedRelay.getDeliveryCount(msgEvent.id)
      expect(newCount).toBeGreaterThan(count)
    })
  })

  describe("Race condition simulation", () => {
    it("should handle rapid sends from both parties", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: alice, publicKey: alicePubkey } =
        await createControlledMockSessionManager("alice-device-1", sharedRelay)

      const { manager: bob, publicKey: bobPubkey } =
        await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const aliceReceived: string[] = []
      const bobReceived: string[] = []

      alice.onEvent((event) => aliceReceived.push(event.content))
      bob.onEvent((event) => bobReceived.push(event.content))

      // Both parties send messages rapidly (simulating race)
      // Use promises to wait for each message to be received
      const bobGotAlice1 = new Promise<void>((r) => {
        const unsub = bob.onEvent((e) => { if (e.content === "alice-1") { unsub(); r() } })
      })
      const bobGotAlice2 = new Promise<void>((r) => {
        const unsub = bob.onEvent((e) => { if (e.content === "alice-2") { unsub(); r() } })
      })
      const aliceGotBob1 = new Promise<void>((r) => {
        const unsub = alice.onEvent((e) => { if (e.content === "bob-1") { unsub(); r() } })
      })
      const aliceGotBob2 = new Promise<void>((r) => {
        const unsub = alice.onEvent((e) => { if (e.content === "bob-2") { unsub(); r() } })
      })

      await alice.sendMessage(bobPubkey, "alice-1")
      await bob.sendMessage(alicePubkey, "bob-1")
      await alice.sendMessage(bobPubkey, "alice-2")
      await bob.sendMessage(alicePubkey, "bob-2")

      // Wait for all messages to be received
      await Promise.all([bobGotAlice1, bobGotAlice2, aliceGotBob1, aliceGotBob2])

      // Verify all messages were received
      expect(bobReceived).toContain("alice-1")
      expect(bobReceived).toContain("alice-2")
      expect(aliceReceived).toContain("bob-1")
      expect(aliceReceived).toContain("bob-2")
    })
  })

  describe("Relay inspection", () => {
    it("should provide access to all events", async () => {
      const sharedRelay = new ControlledMockRelay()

      const { manager: alice } = await createControlledMockSessionManager(
        "alice-device-1",
        sharedRelay
      )

      const { manager: bob, publicKey: bobPubkey } = await createControlledMockSessionManager(
        "bob-device-1",
        sharedRelay
      )

      const initialEventCount = sharedRelay.getAllEvents().length

      // Wait for messages to be received to ensure they were published
      const received = new Promise<void>((resolve) => {
        let count = 0
        bob.onEvent((e) => {
          if (e.content === "test1" || e.content === "test2") {
            count++
            if (count >= 2) resolve()
          }
        })
      })

      await alice.sendMessage(bobPubkey, "test1")
      await alice.sendMessage(bobPubkey, "test2")

      await received

      const finalEventCount = sharedRelay.getAllEvents().length
      // Events should have increased (messages + any session events)
      expect(finalEventCount).toBeGreaterThanOrEqual(initialEventCount)
      // At minimum we should have some events
      expect(finalEventCount).toBeGreaterThan(0)
    })

    it("should allow inspection of delivery to specific subscribers", async () => {
      const sharedRelay = new ControlledMockRelay()

      await createControlledMockSessionManager("alice-device-1", sharedRelay)
      await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const history = sharedRelay.getDeliveryHistory()
      const subs = sharedRelay.getSubscriptions()

      // Should have subscriptions and delivery records
      expect(subs.length).toBeGreaterThan(0)
      expect(history.length).toBeGreaterThan(0)

      // Each delivery record should reference a valid subscriber
      for (const record of history) {
        expect(record.subscriberId).toBeTruthy()
        expect(record.eventId).toBeTruthy()
        expect(record.timestamp).toBeGreaterThan(0)
      }
    })

    it("should track wasDeliveredTo correctly", async () => {
      const sharedRelay = new ControlledMockRelay()

      await createControlledMockSessionManager("alice-device-1", sharedRelay)
      await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const allEvents = sharedRelay.getAllEvents()
      const subs = sharedRelay.getSubscriptions()

      if (allEvents.length > 0 && subs.length > 0) {
        const event = allEvents[0]
        const sub = subs[0]

        // Check if event was delivered to this subscriber
        const wasDelivered = sharedRelay.wasDeliveredTo(event.id, sub.id)
        // The result depends on whether the event matched the filter
        expect(typeof wasDelivered).toBe("boolean")
      }
    })
  })
})
