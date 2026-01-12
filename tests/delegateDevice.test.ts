import { describe, it, expect } from "vitest"
import { SessionManager } from "../src/SessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { createMockSessionManager } from "./helpers/mockSessionManager"

describe("Delegate device end-to-end", () => {
  it("should allow delegate device to receive messages from other users", async () => {
    const sharedRelay = new MockRelay()

    // === STEP 1: Main device (User A) sets up ===
    const { manager: mainDeviceManager, publicKey: userAPubkey } = await createMockSessionManager(
      "main-device",
      sharedRelay
    )

    // === STEP 2: Delegate device creates keys ===
    const { manager: delegateManager, payload: delegatePayload } = SessionManager.createDelegateDevice(
      "delegate-device-1",
      "Delegate Phone",
      (filter, onEvent) => sharedRelay.subscribe(filter, onEvent),
      async (event) => sharedRelay.publish(event)
    )

    // === STEP 3: Main device adds delegate to InviteList ===
    await mainDeviceManager.addDevice({
      ephemeralPubkey: delegatePayload.ephemeralPubkey,
      sharedSecret: delegatePayload.sharedSecret,
      deviceId: delegatePayload.deviceId,
      deviceLabel: delegatePayload.deviceLabel,
      identityPubkey: delegatePayload.identityPubkey,
    })

    // Verify device was added
    const devices = mainDeviceManager.getOwnDevices()
    expect(devices.find(d => d.deviceId === delegatePayload.deviceId)).toBeDefined()

    // === STEP 4: Delegate device initializes ===
    await delegateManager.init()
    expect(delegateManager.isDelegateMode()).toBe(true)

    // === STEP 5: User B wants to chat with User A ===
    const { manager: userBManager } = await createMockSessionManager(
      "user-b-device",
      sharedRelay
    )

    // Set up message collectors
    const mainDeviceMessages: string[] = []
    const delegateDeviceMessages: string[] = []
    const userBMessages: string[] = []

    mainDeviceManager.onEvent((event) => {
      mainDeviceMessages.push(event.content)
    })
    delegateManager.onEvent((event) => {
      delegateDeviceMessages.push(event.content)
    })
    userBManager.onEvent((event) => {
      userBMessages.push(event.content)
    })

    // User B discovers User A's InviteList and accepts invites
    userBManager.setupUser(userAPubkey)

    // Wait for invite acceptance and session establishment
    await new Promise(resolve => setTimeout(resolve, 500))

    // === STEP 6: User B sends a message to User A ===
    await userBManager.sendMessage(userAPubkey, "Hello User A!")

    // Wait for message propagation
    await new Promise(resolve => setTimeout(resolve, 500))

    // === STEP 7: Verify both main device and delegate received the message ===
    expect(mainDeviceMessages).toContain("Hello User A!")
    expect(delegateDeviceMessages).toContain("Hello User A!")

    // Cleanup
    mainDeviceManager.close()
    delegateManager.close()
    userBManager.close()
  }, 10000)

  it("should prevent delegate device from managing devices", async () => {
    const { manager: delegateManager } = SessionManager.createDelegateDevice(
      "delegate-device-1",
      "Delegate Phone",
      () => () => {},
      async () => ({} as any)
    )

    await delegateManager.init()

    // Delegate cannot add devices
    await expect(delegateManager.addDevice({
      ephemeralPubkey: "a".repeat(64),
      sharedSecret: "b".repeat(64),
      deviceId: "c".repeat(16),
      deviceLabel: "Test",
    })).rejects.toThrow(/delegate mode/i)

    // Delegate cannot revoke devices
    await expect(delegateManager.revokeDevice("some-id")).rejects.toThrow(/delegate mode/i)

    // Delegate cannot update device labels
    await expect(delegateManager.updateDeviceLabel("some-id", "new")).rejects.toThrow(/delegate mode/i)

    delegateManager.close()
  })
})
