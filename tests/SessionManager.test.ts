import { describe, it, expect } from "vitest"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { createControlledMockSessionManager } from "./helpers/controlledMockSessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { runScenario } from "./helpers/scenario"

type DeviceRecordSnapshot = { inactiveSessions: unknown[] }

const extractDeviceRecords = (manager: unknown): DeviceRecordSnapshot[] => {
  const internal = manager as {
    userRecords?: Map<string, { devices: Map<string, DeviceRecordSnapshot> }>
  }
  if (!internal.userRecords) return []
  return Array.from(internal.userRecords.values()).flatMap((record) =>
    Array.from(record.devices.values())
  )
}

describe("SessionManager", () => {
  it("should receive a message", async () => {
    const sharedRelay = new MockRelay()

    const { manager: managerAlice, publish: publishAlice } = await createMockSessionManager(
      "alice-device-1",
      sharedRelay
    )

    const { manager: managerBob, publicKey: bobPubkey } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay
    )

    const chatMessage = "Hello Bob from Alice!"

    await managerAlice.sendMessage(bobPubkey, chatMessage)

    expect(publishAlice).toHaveBeenCalled()
    const bobReceivedMessage = await new Promise((resolve) => {
      managerBob.onEvent((event) => {
        if (event.content === chatMessage) resolve(true)
      })
    })
    expect(bobReceivedMessage).toBe(true)
  })

  it("should sync messages across multiple devices", async () => {
    const sharedRelay = new MockRelay()

    const { manager: aliceDevice1, secretKey: aliceSecretKey } =
      await createMockSessionManager("alice-device-1", sharedRelay)

    const { manager: aliceDevice2 } = await createMockSessionManager(
      "alice-device-2",
      sharedRelay,
      aliceSecretKey
    )

    const { manager: bobDevice1, publicKey: bobPubkey } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay
    )

    const msg1 = "Hello Bob from Alice device 1"
    const msg2 = "Hello Bob from Alice device 2"

    await aliceDevice1.sendMessage(bobPubkey, msg1)
    await aliceDevice2.sendMessage(bobPubkey, msg2)

    const bobReceivedMessages = await new Promise((resolve) => {
      const received: string[] = []
      bobDevice1.onEvent((event) => {
        if (event.content === msg1 || event.content === msg2) {
          received.push(event.content)
          if (received.length === 2) resolve(received)
        }
      })
    })

    expect(bobReceivedMessages)
  })

  it("should deliver messages to all sender and recipient devices", async () => {
    await runScenario({
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
          waitOn: "all-recipient-devices",
        },
        { type: "expect", actor: "alice", deviceId: "alice-device-2", message: "alice broadcast" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-2" },
          to: "alice",
          message: "bob broadcast",
          waitOn: "all-recipient-devices",
        },
        { type: "expect", actor: "bob", deviceId: "bob-device-1", message: "bob broadcast" },
        { type: "expect", actor: "alice", deviceId: "alice-device-1", message: "bob broadcast" },
        { type: "expect", actor: "alice", deviceId: "alice-device-2", message: "bob broadcast" },
      ],
    })
  })

  it("should deliver self-sent messages to other online devices", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "alice", deviceId: "alice-device-2" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "alice",
          message: "alice-self-1",
          waitOn: { actor: "alice", deviceId: "alice-device-2" },
        },
        { type: "expect", actor: "alice", deviceId: "alice-device-2", message: "alice-self-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-2" },
          to: "alice",
          message: "alice-self-2",
          waitOn: { actor: "alice", deviceId: "alice-device-1" },
        },
        { type: "expect", actor: "alice", deviceId: "alice-device-1", message: "alice-self-2" },
      ],
    })
  })

  it("should fan out interleaved multi-device messages", async () => {
    const aliceDevice1 = { actor: "alice", deviceId: "alice-device-1" } as const
    const aliceDevice2 = { actor: "alice", deviceId: "alice-device-2" } as const
    const bobDevice1 = { actor: "bob", deviceId: "bob-device-1" } as const
    const bobDevice2 = { actor: "bob", deviceId: "bob-device-2" } as const

    const toBob1 = "a1->bob #1"
    const toAlice1 = "b1->alice"
    const aliceSelf = "a2->alice"
    const bobSelf = "b2->bob"
    const toBob2 = "a1->bob #2"

    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "alice", deviceId: "alice-device-2" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-2" },
        { type: "send", from: aliceDevice1, to: "bob", message: toBob1, waitOn: "all-recipient-devices" },
        { type: "send", from: bobDevice1, to: "alice", message: toAlice1, waitOn: "all-recipient-devices" },
        { type: "send", from: aliceDevice2, to: "alice", message: aliceSelf, waitOn: { actor: "alice", deviceId: "alice-device-1" } },
        { type: "send", from: bobDevice2, to: "bob", message: bobSelf, waitOn: { actor: "bob", deviceId: "bob-device-1" } },
        { type: "send", from: aliceDevice1, to: "bob", message: toBob2, waitOn: "all-recipient-devices" },
        { type: "expectAll", actor: "alice", deviceId: "alice-device-1", messages: [toAlice1, aliceSelf] },
        { type: "expectAll", actor: "alice", deviceId: "alice-device-2", messages: [toBob1, toAlice1, toBob2] },
        { type: "expectAll", actor: "bob", deviceId: "bob-device-1", messages: [toBob1, bobSelf, toBob2] },
        { type: "expectAll", actor: "bob", deviceId: "bob-device-2", messages: [toBob1, toAlice1, toBob2] },
      ],
    })
  })

  it("should handle back to back messages after initial, answer, and then", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "alice to bob 1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "bob to alice 1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "alice to bob 2" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "alice to bob 3" },
      ],
    })
  })

  it("should handle back to back messages after initial", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "Initial message" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "Reply message" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "Reply message 2" },
      ],
    })
  })

  it("should persist sessions across manager restarts", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "Initial message" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "Reply message" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "Reply message 2" },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        { type: "restart", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "Message after restart" },
        { type: "expect", actor: "bob", deviceId: "bob-device-1", message: "Message after restart" },
      ],
    })
  })

  it("should resume communication after restart with stored sessions", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "hello from alice" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "hey alice 1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "hey alice 2" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "hey alice 3" },
        { type: "close", actor: "bob", deviceId: "bob-device-1" },
        { type: "restart", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "hey alice after restart" },
        { type: "expect", actor: "alice", deviceId: "alice-device-1", message: "hey alice after restart" },
      ],
    })
  })

  it("should deliver alice's message after bob restarts", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "alice to bob 1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "bob to alice 1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "alice to bob 2" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "alice to bob 3" },
        { type: "restart", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "bob after restart" },
        { type: "expect", actor: "alice", deviceId: "alice-device-1", message: "bob after restart" },
      ],
    })
  })

  it("should not accumulate additional sessions after restart", async () => {
    const sharedRelay = new MockRelay()

    const {
      manager: aliceManager,
      secretKey: aliceSecretKey,
      publicKey: alicePubkey,
      mockStorage: aliceStorage,
    } = await createMockSessionManager("alice-device-1", sharedRelay)

    const {
      manager: bobManager,
      secretKey: bobSecretKey,
      publicKey: bobPubkey,
      mockStorage: bobStorage,
    } = await createMockSessionManager("bob-device-1", sharedRelay)

    const [msg1, msg2] = ["hello bob", "hello alice"]

    const messagesReceivedBob = new Promise<void>((resolve) => {
      bobManager.onEvent((event) => {
        if (event.content === msg1) {
          resolve()
        }
      })
    })

    const messagesReceivedAlice = new Promise<void>((resolve) => {
      aliceManager.onEvent((event) => {
        if (event.content === msg2) {
          resolve()
        }
      })
    })

    await aliceManager.sendMessage(bobPubkey, msg1)
    await bobManager.sendMessage(alicePubkey, msg2)

    await Promise.all([messagesReceivedBob, messagesReceivedAlice])

    aliceManager.close()
    bobManager.close()

    const { manager: aliceManagerRestart } = await createMockSessionManager(
      "alice-device-1",
      sharedRelay,
      aliceSecretKey,
      aliceStorage
    )

    const { manager: bobManagerRestart } = await createMockSessionManager(
      "bob-device-1",
      sharedRelay,
      bobSecretKey,
      bobStorage
    )

    const afterRestartMessage = "after restart"

    const bobReveivedMessages = new Promise<void>((resolve) => {
      bobManagerRestart.onEvent((event) => {
        if (event.content === afterRestartMessage) {
          resolve()
        }
      })
    })

    await aliceManagerRestart.sendMessage(bobPubkey, "after restart")
    await bobReveivedMessages

    const aliceDeviceRecords = extractDeviceRecords(aliceManagerRestart)
    const bobDeviceRecords = extractDeviceRecords(bobManagerRestart)

    ;[...aliceDeviceRecords, ...bobDeviceRecords].forEach((record) => {
      expect(record.inactiveSessions.length).toBeLessThanOrEqual(1)
    })
  })

  it("should deliver when receiver restarts multiple times", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "2" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "3" },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "4" },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "5" },
      ],
    })
  })

  it("should deliver when receiver restarts multiple times (clearEvents)", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "2" },
        { type: "send", from: { actor: "alice", deviceId: "alice-device-1" }, to: "bob", message: "3" },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "4" },
        { type: "clearEvents" },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        { type: "send", from: { actor: "bob", deviceId: "bob-device-1" }, to: "alice", message: "5" },
      ],
    })
  })
})

describe("SessionManager (Controlled Relay)", () => {
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

      const allEvents = sharedRelay.getAllEvents()
      const msgEvent = allEvents[allEvents.length - 1]

      const count = sharedRelay.getDeliveryCount(msgEvent.id)
      expect(count).toBeGreaterThanOrEqual(1)

      sharedRelay.duplicateEvent(msgEvent.id)

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

      await Promise.all([bobGotAlice1, bobGotAlice2, aliceGotBob1, aliceGotBob2])

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
      expect(finalEventCount).toBeGreaterThanOrEqual(initialEventCount)
      expect(finalEventCount).toBeGreaterThan(0)
    })

    it("should allow inspection of delivery to specific subscribers", async () => {
      const sharedRelay = new ControlledMockRelay()

      await createControlledMockSessionManager("alice-device-1", sharedRelay)
      await createControlledMockSessionManager("bob-device-1", sharedRelay)

      const history = sharedRelay.getDeliveryHistory()
      const subs = sharedRelay.getSubscriptions()

      expect(subs.length).toBeGreaterThan(0)
      expect(history.length).toBeGreaterThan(0)

      for (const record of history) {
        expect(record.subscriberId).toBeTruthy()
        expect(record.eventId).toBeTruthy()
        expect(record.timestamp).toBeGreaterThan(0)
      }
    })
  })
})
