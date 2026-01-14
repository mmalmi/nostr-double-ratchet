import { describe, it } from "vitest"
import { runScenario } from "./helpers/scenario"

describe("Delegate Device Messaging", () => {
  it("alice main → bob: both bob main AND bob delegate should receive", async () => {
    await runScenario({
      steps: [
        // Setup: Alice has main device, Bob has main + delegate
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },
        { type: "addDelegateDevice", actor: "bob", deviceId: "bob-delegate", mainDeviceId: "bob-main" },

        // Alice main sends to Bob
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "hello from alice main",
          waitOn: "all-recipient-devices", // Wait for ALL of Bob's devices
        },

        // Explicitly verify bob-delegate received it
        { type: "expect", actor: "bob", deviceId: "bob-delegate", message: "hello from alice main" },
      ],
    })
  })

  it("alice delegate → bob: both bob main AND bob delegate should receive", async () => {
    await runScenario({
      steps: [
        // Setup: Alice has main + delegate, Bob has main + delegate
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDelegateDevice", actor: "alice", deviceId: "alice-delegate", mainDeviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },
        { type: "addDelegateDevice", actor: "bob", deviceId: "bob-delegate", mainDeviceId: "bob-main" },

        // Alice delegate sends to Bob
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-delegate" },
          to: "bob",
          message: "hello from alice delegate",
          waitOn: "all-recipient-devices",
        },

        // Verify both of Bob's devices received
        { type: "expect", actor: "bob", deviceId: "bob-main", message: "hello from alice delegate" },
        { type: "expect", actor: "bob", deviceId: "bob-delegate", message: "hello from alice delegate" },
      ],
    })
  })

  it("bob replies to alice: alice delegate should receive the reply", async () => {
    await runScenario({
      steps: [
        // Setup
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDelegateDevice", actor: "alice", deviceId: "alice-delegate", mainDeviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },

        // Alice main initiates conversation
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "hi bob",
          waitOn: { actor: "bob", deviceId: "bob-main" },
        },

        // Bob replies
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-main" },
          to: "alice",
          message: "hi alice",
          waitOn: "all-recipient-devices",
        },

        // Alice delegate should have received the reply
        { type: "expect", actor: "alice", deviceId: "alice-delegate", message: "hi alice" },
      ],
    })
  })

  it("bob delegate replies to alice: alice delegate should receive", async () => {
    await runScenario({
      steps: [
        // Setup: both have main + delegate
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDelegateDevice", actor: "alice", deviceId: "alice-delegate", mainDeviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },
        { type: "addDelegateDevice", actor: "bob", deviceId: "bob-delegate", mainDeviceId: "bob-main" },

        // Alice main initiates
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "starting convo",
          waitOn: "all-recipient-devices",
        },

        // Bob delegate replies
        {
          type: "send",
          from: { actor: "bob", deviceId: "bob-delegate" },
          to: "alice",
          message: "reply from bob delegate",
          waitOn: "all-recipient-devices",
        },

        // Both alice devices should receive
        { type: "expect", actor: "alice", deviceId: "alice-main", message: "reply from bob delegate" },
        { type: "expect", actor: "alice", deviceId: "alice-delegate", message: "reply from bob delegate" },
      ],
    })
  })

  it("messages sync to sender's own delegate device", async () => {
    await runScenario({
      steps: [
        // Setup
        { type: "addDevice", actor: "alice", deviceId: "alice-main" },
        { type: "addDelegateDevice", actor: "alice", deviceId: "alice-delegate", mainDeviceId: "alice-main" },
        { type: "addDevice", actor: "bob", deviceId: "bob-main" },

        // Alice main sends to Bob
        {
          type: "send",
          from: { actor: "alice", deviceId: "alice-main" },
          to: "bob",
          message: "synced msg",
          waitOn: { actor: "bob", deviceId: "bob-main" },
        },

        // Alice's delegate should also see the message (for multi-device sync)
        { type: "expect", actor: "alice", deviceId: "alice-delegate", message: "synced msg" },
      ],
    })
  })
})
