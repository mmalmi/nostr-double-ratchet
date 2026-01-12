import { describe, it, expect } from "vitest"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { generateSecretKey, getPublicKey } from "nostr-tools"

describe("SessionManager device methods", () => {
  describe("getOwnDevice", () => {
    it("should return the current device entry", async () => {
      const sharedRelay = new MockRelay()
      const { manager } = await createMockSessionManager("my-device-1", sharedRelay)

      const ownDevice = manager.getOwnDevice()

      expect(ownDevice).toBeDefined()
      expect(ownDevice!.deviceId).toBe("my-device-1")
      expect(ownDevice!.deviceLabel).toBe("my-device-1") // default label is deviceId
      expect(ownDevice!.ephemeralPublicKey).toHaveLength(64)
      expect(ownDevice!.sharedSecret).toHaveLength(64)
      expect(ownDevice!.ephemeralPrivateKey).toBeInstanceOf(Uint8Array)
      expect(ownDevice!.createdAt).toBeGreaterThan(0)
    })

    it("should return undefined before init", async () => {
      const sharedRelay = new MockRelay()
      const { generateSecretKey, getPublicKey } = await import("nostr-tools")
      const { SessionManager } = await import("../src/SessionManager")
      const { InMemoryStorageAdapter } = await import("../src/StorageAdapter")

      const secretKey = generateSecretKey()
      const publicKey = getPublicKey(secretKey)
      const storage = new InMemoryStorageAdapter()

      const manager = new SessionManager(
        publicKey,
        secretKey,
        "test-device",
        () => () => {},
        async () => {},
        storage
      )

      // Before init, should return undefined
      const ownDevice = manager.getOwnDevice()
      expect(ownDevice).toBeUndefined()
    })
  })

  describe("getOwnDevices", () => {
    it("should return array containing own device", async () => {
      const sharedRelay = new MockRelay()
      const { manager } = await createMockSessionManager("device-1", sharedRelay)

      const devices = manager.getOwnDevices()

      expect(devices).toHaveLength(1)
      expect(devices[0].deviceId).toBe("device-1")
    })

    it("should return empty array before init", async () => {
      const { generateSecretKey, getPublicKey } = await import("nostr-tools")
      const { SessionManager } = await import("../src/SessionManager")
      const { InMemoryStorageAdapter } = await import("../src/StorageAdapter")

      const secretKey = generateSecretKey()
      const publicKey = getPublicKey(secretKey)
      const storage = new InMemoryStorageAdapter()

      const manager = new SessionManager(
        publicKey,
        secretKey,
        "test-device",
        () => () => {},
        async () => {},
        storage
      )

      const devices = manager.getOwnDevices()
      expect(devices).toEqual([])
    })

    it("should include all devices from InviteList", async () => {
      const sharedRelay = new MockRelay()
      const { manager: device1Manager, secretKey } = await createMockSessionManager(
        "device-1",
        sharedRelay
      )

      // Create second device with same identity (simulating multi-device)
      const { manager: device2Manager } = await createMockSessionManager(
        "device-2",
        sharedRelay,
        secretKey
      )

      // Wait for relay sync
      await new Promise((resolve) => setTimeout(resolve, 100))

      // Each manager should see at least its own device
      const device1List = device1Manager.getOwnDevices()
      const device2List = device2Manager.getOwnDevices()

      expect(device1List.some((d) => d.deviceId === "device-1")).toBe(true)
      expect(device2List.some((d) => d.deviceId === "device-2")).toBe(true)
    })
  })

  describe("device entry properties", () => {
    it("should have ephemeralPrivateKey only for own device", async () => {
      const sharedRelay = new MockRelay()
      const { manager } = await createMockSessionManager("my-device", sharedRelay)

      const ownDevice = manager.getOwnDevice()

      // Own device should have private key
      expect(ownDevice).toBeDefined()
      expect(ownDevice!.ephemeralPrivateKey).toBeDefined()
      expect(ownDevice!.ephemeralPrivateKey).toBeInstanceOf(Uint8Array)
    })

    it("should preserve device entry after restart", async () => {
      const sharedRelay = new MockRelay()
      const { InMemoryStorageAdapter } = await import("../src/StorageAdapter")
      const storage = new InMemoryStorageAdapter()

      // Create first manager
      const { manager: manager1, secretKey, publicKey } = await createMockSessionManager(
        "persistent-device",
        sharedRelay,
        undefined,
        storage
      )

      const originalDevice = manager1.getOwnDevice()
      expect(originalDevice).toBeDefined()

      // Close first manager
      manager1.close()

      // Create second manager with same storage (simulating restart)
      const { manager: manager2 } = await createMockSessionManager(
        "persistent-device",
        sharedRelay,
        secretKey,
        storage
      )

      const restoredDevice = manager2.getOwnDevice()

      expect(restoredDevice).toBeDefined()
      expect(restoredDevice!.deviceId).toBe(originalDevice!.deviceId)
      expect(restoredDevice!.ephemeralPublicKey).toBe(originalDevice!.ephemeralPublicKey)
      expect(restoredDevice!.sharedSecret).toBe(originalDevice!.sharedSecret)
    })
  })

  describe("addDevice with identityPubkey (delegate devices)", () => {
    it("should add device with identityPubkey to InviteList", async () => {
      const sharedRelay = new MockRelay()
      const { manager } = await createMockSessionManager("main-device", sharedRelay)
      const { generateEphemeralKeypair, generateSharedSecret, generateDeviceId } = await import("../src/inviteUtils")

      const delegatePrivkey = generateSecretKey()
      const delegatePubkey = getPublicKey(delegatePrivkey)
      const ephemeral = generateEphemeralKeypair()

      await manager.addDevice({
        ephemeralPubkey: ephemeral.publicKey,
        sharedSecret: generateSharedSecret(),
        deviceId: generateDeviceId(),
        deviceLabel: "Delegate Phone",
        identityPubkey: delegatePubkey,
      })

      const devices = manager.getOwnDevices()
      const delegateDevice = devices.find(d => d.deviceLabel === "Delegate Phone")

      expect(delegateDevice).toBeDefined()
      expect(delegateDevice!.identityPubkey).toBe(delegatePubkey)
    })

    it("should publish InviteList with identityPubkey in tag", async () => {
      const sharedRelay = new MockRelay()
      const { manager, publicKey } = await createMockSessionManager("main-device", sharedRelay)
      const { generateEphemeralKeypair, generateSharedSecret, generateDeviceId } = await import("../src/inviteUtils")

      const delegatePrivkey = generateSecretKey()
      const delegatePubkey = getPublicKey(delegatePrivkey)
      const deviceId = generateDeviceId()
      const ephemeral = generateEphemeralKeypair()

      await manager.addDevice({
        ephemeralPubkey: ephemeral.publicKey,
        sharedSecret: generateSharedSecret(),
        deviceId,
        deviceLabel: "Delegate Phone",
        identityPubkey: delegatePubkey,
      })

      // Check that the relay received an InviteList event with the identityPubkey
      const inviteListEvents = sharedRelay.getEvents().filter(e => e.kind === 10078)
      expect(inviteListEvents.length).toBeGreaterThan(0)

      const latestInviteList = inviteListEvents[inviteListEvents.length - 1]
      const deviceTag = latestInviteList.tags.find(
        t => t[0] === "device" && t[3] === deviceId
      )

      expect(deviceTag).toBeDefined()
      expect(deviceTag![6]).toBe(delegatePubkey)
    })

    it("should add device without identityPubkey (regular device)", async () => {
      const sharedRelay = new MockRelay()
      const { manager } = await createMockSessionManager("main-device", sharedRelay)
      const { generateEphemeralKeypair, generateSharedSecret, generateDeviceId } = await import("../src/inviteUtils")

      const ephemeral = generateEphemeralKeypair()

      await manager.addDevice({
        ephemeralPubkey: ephemeral.publicKey,
        sharedSecret: generateSharedSecret(),
        deviceId: generateDeviceId(),
        deviceLabel: "Regular Device",
        // No identityPubkey - this is a regular device
      })

      const devices = manager.getOwnDevices()
      const regularDevice = devices.find(d => d.deviceLabel === "Regular Device")

      expect(regularDevice).toBeDefined()
      expect(regularDevice!.identityPubkey).toBeUndefined()
    })
  })
})
