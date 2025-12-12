import { describe, it, expect } from "vitest"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"
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
          waitOn: "all-recipient-devices",
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
        {
          type: "expect",
          actor: "alice",
          deviceId: "alice-device-2",
          message: "alice-self-1",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-2" },
          to: "alice",
          message: "alice-self-2",
          waitOn: { actor: "alice", deviceId: "alice-device-1" },
        },
        {
          type: "expect",
          actor: "alice",
          deviceId: "alice-device-1",
          message: "alice-self-2",
        },
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
        {
          type: "send",
          from: aliceDevice1,
          to: "bob",
          message: toBob1,
          waitOn: "all-recipient-devices",
        },
        {
          type: "send",
          from: bobDevice1,
          to: "alice",
          message: toAlice1,
          waitOn: "all-recipient-devices",
        },
        {
          type: "send",
          from: aliceDevice2,
          to: "alice",
          message: aliceSelf,
          waitOn: { actor: "alice", deviceId: "alice-device-1" },
        },
        {
          type: "send",
          from: bobDevice2,
          to: "bob",
          message: bobSelf,
          waitOn: { actor: "bob", deviceId: "bob-device-1" },
        },
        {
          type: "send",
          from: aliceDevice1,
          to: "bob",
          message: toBob2,
          waitOn: "all-recipient-devices",
        },
        {
          type: "expectAll",
          actor: "alice",
          deviceId: "alice-device-1",
          messages: [toAlice1, aliceSelf],
        },
        {
          type: "expectAll",
          actor: "alice",
          deviceId: "alice-device-2",
          messages: [toBob1, toAlice1, toBob2],
        },
        {
          type: "expectAll",
          actor: "bob",
          deviceId: "bob-device-1",
          messages: [toBob1, bobSelf, toBob2],
        },
        {
          type: "expectAll",
          actor: "bob",
          deviceId: "bob-device-2",
          messages: [toBob1, toAlice1, toBob2],
        },
      ],
    })
  })

  it("should handle back to back messages after initial, answer, and then", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "alice to bob 1",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "bob to alice 1",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "alice to bob 2",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "alice to bob 3",
        },
      ],
    })
  })
  it("should handle back to back messages after initial", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "Initial message",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "Reply message",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "Reply message 2",
        },
      ],
    })
  })

  it("should persist sessions across manager restarts", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "Initial message",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "Reply message",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "Reply message 2",
        },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        { type: "restart", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "Message after restart",
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

  it("should resume communication after restart with stored sessions", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "hello from alice",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "hey alice 1",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "hey alice 2",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "hey alice 3",
        },
        { type: "close", actor: "bob", deviceId: "bob-device-1" },
        { type: "restart", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "hey alice after restart",
        },
        {
          type: "expect",
          actor: "alice",
          deviceId: "alice-device-1",
          message: "hey alice after restart",
        },
      ],
    })
  })

  it("should deliver alice's message after bob restarts", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "alice to bob 1",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "bob to alice 1",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "alice to bob 2",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "alice to bob 3",
        },
        { type: "restart", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "bob after restart",
        },
        {
          type: "expect",
          actor: "alice",
          deviceId: "alice-device-1",
          message: "bob after restart",
        },
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

    console.log("\n\n\nClosed managers")

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

    console.log("Restarted managers")

    const afterRestartMessage = "after restart"

    console.log("Sending message after restart")
    const bobReveivedMessages = new Promise<void>((resolve) => {
      bobManagerRestart.onEvent((event) => {
        if (event.content === afterRestartMessage) {
          resolve()
        }
      })
    })

    await aliceManagerRestart.sendMessage(bobPubkey, "after restart")
    console.log("Message sent after restart")
    await bobReveivedMessages

    console.log("Message received after restart")
    const aliceDeviceRecords = extractDeviceRecords(aliceManagerRestart)
    const bobDeviceRecords = extractDeviceRecords(bobManagerRestart)

    console.log("a", aliceDeviceRecords)
    console.log("b", bobDeviceRecords)
    ;[...aliceDeviceRecords, ...bobDeviceRecords].forEach((record) => {
      expect(record.inactiveSessions.length).toBeLessThanOrEqual(1)
    })
  })

  it("should deliver when receiver restarts multiple times", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "1",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "2",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "3",
        },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "4",
        },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "5",
        },
      ],
    })
  })

  it("should deliver when receiver restarts multiple times (clearEvents)", async () => {
    await runScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-device-1" },
        { type: "addDevice", actor: "bob", deviceId: "bob-device-1" },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "1",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "2",
        },
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-device-1" },
          to: "bob",
          message: "3",
        },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "4",
        },
        { type: "clearEvents" },
        { type: "restart", actor: "alice", deviceId: "alice-device-1" },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-device-1" },
          to: "alice",
          message: "5",
        },
      ],
    })
  })
})
