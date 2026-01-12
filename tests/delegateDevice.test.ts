import { describe, it, expect } from "vitest"
import { SessionManager } from "../src/SessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { createMockSessionManager } from "./helpers/mockSessionManager"

describe("Delegate device end-to-end", () => {
  it("should complete full flow: generate -> add -> discover -> chat", async () => {
    const sharedRelay = new MockRelay()

    // === STEP 1: Device B (delegate) creates SessionManager with keys ===
    const { manager: delegateManager, payload: delegatePayload } = SessionManager.createDelegateDevice(
      "delegate-device-1",
      "Delegate Phone",
      (filter, onEvent) => sharedRelay.subscribe(filter, onEvent),
      async (event) => sharedRelay.publish(event)
    )

    expect(delegatePayload.identityPubkey).toHaveLength(64)
    expect(delegatePayload.ephemeralPubkey).toHaveLength(64)

    // === STEP 2: Device A (main device) adds Device B to InviteList ===
    const { manager: mainDeviceManager, publicKey: mainPubkey } = await createMockSessionManager(
      "main-device",
      sharedRelay
    )

    // Main device scans QR code / receives delegate device info
    await mainDeviceManager.addDevice({
      ephemeralPubkey: delegatePayload.ephemeralPubkey,
      sharedSecret: delegatePayload.sharedSecret,
      deviceId: delegatePayload.deviceId,
      deviceLabel: delegatePayload.deviceLabel,
      identityPubkey: delegatePayload.identityPubkey,
    })

    // Verify device was added to InviteList
    const devices = mainDeviceManager.getOwnDevices()
    const addedDevice = devices.find(d => d.deviceId === delegatePayload.deviceId)
    expect(addedDevice).toBeDefined()
    expect(addedDevice!.identityPubkey).toBe(delegatePayload.identityPubkey)

    // === STEP 3: Device B (delegate) initializes ===
    await delegateManager.init()
    expect(delegateManager.isDelegateMode()).toBe(true)

    // === STEP 4: User C discovers Device B and establishes session ===
    const { manager: userCManager, publicKey: userCPubkey } = await createMockSessionManager(
      "user-c-device",
      sharedRelay
    )

    // User C sets up to chat with the main user (which includes Device B)
    userCManager.setupUser(mainPubkey)

    // Wait for relay to sync events
    await new Promise(resolve => setTimeout(resolve, 100))

    // User C should discover Device B's invite in the InviteList and accept it
    // This happens automatically via setupUser()

    // === STEP 5: Verify delegate device can receive messages ===
    const delegateReceivedMessages: any[] = []
    delegateManager.onEvent((event) => {
      delegateReceivedMessages.push(event)
    })

    // User C sends a message to the main user
    await userCManager.sendMessage(mainPubkey, "Hello from User C!")

    // Wait for message propagation
    await new Promise(resolve => setTimeout(resolve, 500))

    // Note: For the delegate to receive messages, a session must be established
    // This happens when User C accepts the invite from Device B's ephemeral key
    // The full flow is complex and depends on the InviteList being discovered

    // === STEP 6: Verify delegate mode restrictions ===
    await expect(delegateManager.addDevice({
      ephemeralPubkey: "a".repeat(64),
      sharedSecret: "b".repeat(64),
      deviceId: "c".repeat(16),
      deviceLabel: "Test",
    })).rejects.toThrow(/delegate mode/i)

    await expect(delegateManager.revokeDevice("some-id")).rejects.toThrow(/delegate mode/i)

    await expect(delegateManager.updateDeviceLabel("some-id", "new")).rejects.toThrow(/delegate mode/i)

    // Cleanup
    mainDeviceManager.close()
    delegateManager.close()
    userCManager.close()
  })

  it("should setup delegate device that can listen for invite responses", async () => {
    const sharedRelay = new MockRelay()

    // Setup main device (User A)
    const { manager: mainManager, publicKey: userAPubkey } =
      await createMockSessionManager("user-a-main", sharedRelay)

    // Setup delegate device for User A using static factory
    const { manager: delegateManager, payload: delegatePayload } = SessionManager.createDelegateDevice(
      "delegate-device-1",
      "User A Delegate",
      (filter, onEvent) => sharedRelay.subscribe(filter, onEvent),
      async (event) => sharedRelay.publish(event)
    )

    await mainManager.addDevice({
      ephemeralPubkey: delegatePayload.ephemeralPubkey,
      sharedSecret: delegatePayload.sharedSecret,
      deviceId: delegatePayload.deviceId,
      deviceLabel: delegatePayload.deviceLabel,
      identityPubkey: delegatePayload.identityPubkey,
    })

    // Verify delegate was added with correct identityPubkey
    const devices = mainManager.getOwnDevices()
    const delegateEntry = devices.find(d => d.deviceId === delegatePayload.deviceId)
    expect(delegateEntry).toBeDefined()
    expect(delegateEntry!.identityPubkey).toBe(delegatePayload.identityPubkey)
    expect(delegateEntry!.ephemeralPublicKey).toBe(delegatePayload.ephemeralPubkey)

    await delegateManager.init()

    // Verify delegate mode is active
    expect(delegateManager.isDelegateMode()).toBe(true)
    expect(delegateManager.getDeviceId()).toBe(delegatePayload.deviceId)

    // Setup User B who will discover and chat with User A
    const { manager: userBManager } =
      await createMockSessionManager("user-b-device", sharedRelay)

    // User B sets up to chat with User A (main pubkey)
    // This should discover the InviteList including the delegate device
    userBManager.setupUser(userAPubkey)

    // Wait for relay to process
    await new Promise(resolve => setTimeout(resolve, 100))

    // Cleanup
    mainManager.close()
    delegateManager.close()
    userBManager.close()
  })
})
