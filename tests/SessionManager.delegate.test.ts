import { describe, it, expect, vi } from "vitest"
import { SessionManager } from "../src/SessionManager"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { MockRelay } from "./helpers/mockRelay"
import { createMockSessionManager } from "./helpers/mockSessionManager"

describe("SessionManager delegate mode", () => {
  describe("initialization", () => {
    it("should create manager in delegate mode using static factory", async () => {
      const { manager, payload } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        () => () => {},
        async () => ({} as any),
        new InMemoryStorageAdapter()
      )

      expect(manager.isDelegateMode()).toBe(true)
      expect(payload.deviceId).toBe("delegate-device-1")
      expect(payload.deviceLabel).toBe("Delegate Phone")
      expect(payload.ephemeralPubkey).toHaveLength(64)
      expect(payload.sharedSecret).toHaveLength(64)
      expect(payload.identityPubkey).toHaveLength(64)
    })

    it("should not publish InviteList on init in delegate mode", async () => {
      const mockRelay = new MockRelay()
      const publish = vi.fn().mockResolvedValue({} as any)

      const { manager } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        (filter, onEvent) => mockRelay.subscribe(filter, onEvent),
        publish,
        new InMemoryStorageAdapter()
      )

      await manager.init()

      // Should not publish any InviteList events
      const inviteListPublishes = publish.mock.calls.filter(
        (call: any) => call[0]?.kind === 10078
      )
      expect(inviteListPublishes).toHaveLength(0)
    })

    it("should return true for isDelegateMode() in delegate mode", async () => {
      const { manager } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        () => () => {},
        async () => ({} as any),
        new InMemoryStorageAdapter()
      )

      expect(manager.isDelegateMode()).toBe(true)
    })

    it("should return false for isDelegateMode() in normal mode", async () => {
      const sharedRelay = new MockRelay()
      const { manager } = await createMockSessionManager("main-device", sharedRelay)

      expect(manager.isDelegateMode()).toBe(false)
    })
  })

  describe("restrictions", () => {
    it("should throw on addDevice() in delegate mode", async () => {
      const { manager } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        () => () => {},
        async () => ({} as any),
        new InMemoryStorageAdapter()
      )

      await manager.init()

      await expect(
        manager.addDevice({
          ephemeralPubkey: "a".repeat(64),
          sharedSecret: "b".repeat(64),
          deviceId: "c".repeat(16),
          deviceLabel: "Test",
        })
      ).rejects.toThrow(/delegate mode/i)
    })

    it("should throw on revokeDevice() in delegate mode", async () => {
      const { manager } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        () => () => {},
        async () => ({} as any),
        new InMemoryStorageAdapter()
      )

      await manager.init()

      await expect(
        manager.revokeDevice("some-device-id")
      ).rejects.toThrow(/delegate mode/i)
    })

    it("should throw on updateDeviceLabel() in delegate mode", async () => {
      const { manager } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        () => () => {},
        async () => ({} as any),
        new InMemoryStorageAdapter()
      )

      await manager.init()

      await expect(
        manager.updateDeviceLabel("some-device-id", "New Label")
      ).rejects.toThrow(/delegate mode/i)
    })
  })

  describe("listening for invites", () => {
    it("should listen for invite responses on ephemeral key", async () => {
      const mockRelay = new MockRelay()

      const subscribeFilters: any[] = []
      const subscribe = vi.fn().mockImplementation((filter, onEvent) => {
        subscribeFilters.push(filter)
        return mockRelay.subscribe(filter, onEvent)
      })

      const { manager, payload } = SessionManager.createDelegateDevice(
        "delegate-device-1",
        "Delegate Phone",
        subscribe,
        async () => ({} as any),
        new InMemoryStorageAdapter()
      )

      await manager.init()

      // Should have subscribed to invite responses on the ephemeral key
      const inviteResponseFilter = subscribeFilters.find(
        f => f.kinds?.includes(1059) && f["#p"]?.includes(payload.ephemeralPubkey)
      )
      expect(inviteResponseFilter).toBeDefined()
    })
  })
})
