import { describe, it } from "vitest"
import { runControlledScenario } from "./helpers/controlledScenario"

/**
 * Tests that the persistent MessageQueue + DiscoveryQueue survive crash/restart
 * and deliver queued messages once the session is (re-)established.
 */
describe("MessageQueue crash recovery", () => {
  it("queued message delivers after sender restart", async () => {
    await runControlledScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },

        // Establish session
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "init",
          waitOn: "auto",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-main" },
          to: "alice",
          message: "ack",
          waitOn: "auto",
        },

        // Queue a message without waiting for delivery
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "before-crash",
        },

        // Crash & restart alice
        { type: "close", actor: "alice", deviceId: "alice-main" },
        { type: "restart", actor: "alice", deviceId: "alice-main" },

        // Let everything flush
        { type: "deliverAll" },

        // Bob should get the message that was queued before the crash
        { type: "expect", actor: "bob", deviceId: "bob-main", message: "before-crash" },
      ],
    })
  })

  it("queued message delivers after recipient restart", async () => {
    await runControlledScenario({
      steps: [
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },

        // Establish session
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "init",
          waitOn: "auto",
        },
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-main" },
          to: "alice",
          message: "ack",
          waitOn: "auto",
        },

        // Close bob (simulate crash)
        { type: "close", actor: "bob", deviceId: "bob-main" },

        // Alice sends while bob is offline
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "while-bob-offline",
        },

        // Bob comes back
        { type: "restart", actor: "bob", deviceId: "bob-main" },

        // Flush
        { type: "deliverAll" },

        // Bob should receive the message
        { type: "expect", actor: "bob", deviceId: "bob-main", message: "while-bob-offline" },
      ],
    })
  })

  it("message queued before any session survives sender restart", async () => {
    await runControlledScenario({
      steps: [
        // Only add alice — bob doesn't exist yet so discovery can't find anything
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },

        // Send when bob has no device — message goes to discoveryQueue with no
        // possibility of session establishment (bob's AppKeys aren't on the relay)
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "no-session-yet",
        },

        // Crash & restart alice (queue must survive via storage)
        { type: "close", actor: "alice", deviceId: "alice-main" },
        { type: "restart", actor: "alice", deviceId: "alice-main" },

        // NOW bob comes online — his AppKeys + Invite appear on the relay
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },

        // Flush everything — alice discovers bob, establishes session, drains queue
        { type: "deliverAll" },

        // Bob should receive the pre-session message
        { type: "expect", actor: "bob", deviceId: "bob-main", message: "no-session-yet" },
      ],
    })
  })
})
