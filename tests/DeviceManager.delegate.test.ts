import { describe, it, expect, vi, beforeEach } from "vitest"
import { DeviceManager } from "../src/DeviceManager"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND } from "../src/types"
import { generateSecretKey, getPublicKey, finalizeEvent } from "nostr-tools"
import { bytesToHex } from "@noble/hashes/utils"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"

describe("DeviceManager - Delegate Device", () => {
  let nostrSubscribe: NostrSubscribe
  let nostrPublish: NostrPublish
  let publishedEvents: any[]
  let subscriptions: Map<string, (event: any) => void>

  beforeEach(() => {
    publishedEvents = []
    subscriptions = new Map()

    nostrSubscribe = vi.fn((filter, onEvent) => {
      const key = JSON.stringify(filter)
      subscriptions.set(key, onEvent)
      return () => {
        subscriptions.delete(key)
      }
    }) as unknown as NostrSubscribe

    nostrPublish = vi.fn(async (event) => {
      publishedEvents.push(event)
      return event
    }) as unknown as NostrPublish
  })

  describe("createDelegate()", () => {
    it("should create a DeviceManager in delegate mode", () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(DeviceManager)
    })

    it("should return isDelegateMode() === true", () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager.isDelegateMode()).toBe(true)
    })

    it("should generate identity keypair", () => {
      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      // Identity pubkey should be in payload
      expect(payload.identityPubkey).toBeDefined()
      expect(payload.identityPubkey).toHaveLength(64) // hex pubkey

      // Manager should return the same identity
      expect(manager.getIdentityPublicKey()).toBe(payload.identityPubkey)

      // Private key should be available
      const privkey = manager.getIdentityPrivateKey()
      expect(privkey).toBeInstanceOf(Uint8Array)
      expect(privkey.length).toBe(32)
    })

    it("should generate ephemeral keypair", () => {
      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      // Ephemeral pubkey should be in payload
      expect(payload.ephemeralPubkey).toBeDefined()
      expect(payload.ephemeralPubkey).toHaveLength(64)

      // Manager should return the keypair
      const keypair = manager.getEphemeralKeypair()
      expect(keypair).not.toBeNull()
      expect(keypair?.publicKey).toBe(payload.ephemeralPubkey)
      expect(keypair?.privateKey).toBeInstanceOf(Uint8Array)
    })

    it("should generate shared secret", () => {
      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      // Shared secret should be in payload
      expect(payload.sharedSecret).toBeDefined()
      expect(payload.sharedSecret).toHaveLength(64)

      // Manager should return the same secret
      expect(manager.getSharedSecret()).toBe(payload.sharedSecret)
    })

    it("should return payload with public keys", () => {
      const { payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      expect(payload.deviceId).toBe("delegate-device")
      expect(payload.deviceLabel).toBe("My Phone")
      expect(payload.ephemeralPubkey).toBeDefined()
      expect(payload.sharedSecret).toBeDefined()
      expect(payload.identityPubkey).toBeDefined()
    })
  })

  describe("init()", () => {
    it("should NOT publish InviteList", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should not have published any InviteList events
      const inviteListEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      )
      expect(inviteListEvents.length).toBe(0)
    })

    it("should load stored owner pubkey if exists", async () => {
      const storage = new InMemoryStorageAdapter()
      const ownerPubkey = getPublicKey(generateSecretKey())

      // Pre-store owner pubkey
      await storage.put("v1/device-manager/owner-pubkey", ownerPubkey)

      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()

      expect(manager.getOwnerPublicKey()).toBe(ownerPubkey)
    })
  })

  describe("waitForActivation()", () => {
    it("should subscribe to InviteList events", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Start waiting (don't await, it will timeout)
      const activationPromise = manager.waitForActivation(100)

      // Should have subscribed to InviteList events
      expect(nostrSubscribe).toHaveBeenCalled()
      const calls = (nostrSubscribe as any).mock.calls
      const inviteListCall = calls.find(
        (call: any) => call[0].kinds?.includes(INVITE_LIST_EVENT_KIND)
      )
      expect(inviteListCall).toBeDefined()

      // Let it timeout
      await expect(activationPromise).rejects.toThrow("Activation timeout")
    })

    it("should resolve when own deviceId appears in an InviteList", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Start waiting
      const activationPromise = manager.waitForActivation(5000)

      // Simulate owner adding this device to their InviteList
      await new Promise((resolve) => setTimeout(resolve, 50))

      // Create a signed InviteList event containing this device
      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            [
              "device",
              payload.ephemeralPubkey,
              payload.sharedSecret,
              payload.deviceId,
              payload.deviceLabel,
              String(Math.floor(Date.now() / 1000)),
              payload.identityPubkey!,
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      // Find the subscription and trigger it
      const subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        const callback = subscriptions.get(subscriptionKey)
        callback?.(inviteListEvent)
      }

      const result = await activationPromise
      expect(result).toBe(ownerPublicKey)
    })

    it("should return the owner pubkey who added this device", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const activationPromise = manager.waitForActivation(5000)

      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            [
              "device",
              payload.ephemeralPubkey,
              payload.sharedSecret,
              payload.deviceId,
              payload.deviceLabel,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      const subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      const ownerPubkey = await activationPromise
      expect(ownerPubkey).toBe(ownerPublicKey)
    })

    it("should store owner pubkey for future use", async () => {
      const storage = new InMemoryStorageAdapter()
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()
      const activationPromise = manager.waitForActivation(5000)

      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            [
              "device",
              payload.ephemeralPubkey,
              payload.sharedSecret,
              payload.deviceId,
              payload.deviceLabel,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      const subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      await activationPromise

      // Check storage
      const storedOwnerPubkey = await storage.get<string>(
        "v1/device-manager/owner-pubkey"
      )
      expect(storedOwnerPubkey).toBe(ownerPublicKey)
    })

    it("should timeout if not activated within timeoutMs", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      await expect(manager.waitForActivation(100)).rejects.toThrow(
        "Activation timeout"
      )
    })

    it("should resolve immediately if already activated", async () => {
      const storage = new InMemoryStorageAdapter()
      const ownerPubkey = getPublicKey(generateSecretKey())

      // Pre-store owner pubkey (simulating previous activation)
      await storage.put("v1/device-manager/owner-pubkey", ownerPubkey)

      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()

      // Should resolve immediately since already activated
      const result = await manager.waitForActivation(100)
      expect(result).toBe(ownerPubkey)
    })
  })

  describe("getOwnerPublicKey()", () => {
    it("should return null before activation", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      expect(manager.getOwnerPublicKey()).toBeNull()
    })

    it("should return owner pubkey after activation", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const activationPromise = manager.waitForActivation(5000)

      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            [
              "device",
              payload.ephemeralPubkey,
              payload.sharedSecret,
              payload.deviceId,
              payload.deviceLabel,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      const subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      await activationPromise

      expect(manager.getOwnerPublicKey()).toBe(ownerPublicKey)
    })
  })

  describe("isRevoked()", () => {
    it("should return false when device is in InviteList", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Activate the device
      const activationPromise = manager.waitForActivation(5000)
      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            [
              "device",
              payload.ephemeralPubkey,
              payload.sharedSecret,
              payload.deviceId,
              payload.deviceLabel,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      let subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      await activationPromise

      // Now check revocation - mock the fetch to return the same InviteList
      const isRevokedSubscribe = vi.fn((filter, onEvent) => {
        if (
          filter.kinds?.includes(INVITE_LIST_EVENT_KIND) &&
          filter.authors?.includes(ownerPublicKey)
        ) {
          setTimeout(() => onEvent(inviteListEvent), 10)
        }
        return () => {}
      }) as unknown as NostrSubscribe

      // Replace the subscribe for isRevoked check
      ;(manager as any).nostrSubscribe = isRevokedSubscribe

      const revoked = await manager.isRevoked()
      expect(revoked).toBe(false)
    })

    it("should return true when device is removed from InviteList", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Activate the device
      const activationPromise = manager.waitForActivation(5000)
      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            [
              "device",
              payload.ephemeralPubkey,
              payload.sharedSecret,
              payload.deviceId,
              payload.deviceLabel,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      let subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      await activationPromise

      // Create a new InviteList WITHOUT this device (simulating revocation)
      const revokedInviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          pubkey: ownerPublicKey,
          created_at: Math.floor(Date.now() / 1000) + 1,
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["removed", payload.deviceId],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      // Mock the fetch to return the revoked InviteList
      const isRevokedSubscribe = vi.fn((filter, onEvent) => {
        if (
          filter.kinds?.includes(INVITE_LIST_EVENT_KIND) &&
          filter.authors?.includes(ownerPublicKey)
        ) {
          setTimeout(() => onEvent(revokedInviteListEvent), 10)
        }
        return () => {}
      }) as unknown as NostrSubscribe

      ;(manager as any).nostrSubscribe = isRevokedSubscribe

      const revoked = await manager.isRevoked()
      expect(revoked).toBe(true)
    })
  })

  describe("restrictions", () => {
    it("addDevice() should throw in delegate mode", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      await expect(
        manager.addDevice({
          ephemeralPubkey: getPublicKey(generateSecretKey()),
          sharedSecret: bytesToHex(generateSecretKey()),
          deviceId: "another-device",
          deviceLabel: "Another Device",
        })
      ).rejects.toThrow("Cannot add devices in delegate mode")
    })

    it("revokeDevice() should throw in delegate mode", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      await expect(manager.revokeDevice("some-device")).rejects.toThrow(
        "Cannot revoke devices in delegate mode"
      )
    })

    it("updateDeviceLabel() should throw in delegate mode", async () => {
      const { manager } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      await expect(
        manager.updateDeviceLabel("some-device", "New Label")
      ).rejects.toThrow("Cannot update device labels in delegate mode")
    })
  })

  describe("getters", () => {
    it("getIdentityPublicKey() should return delegate device pubkey", () => {
      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager.getIdentityPublicKey()).toBe(payload.identityPubkey)
    })

    it("getIdentityPrivateKey() should return delegate device privkey", () => {
      const { manager, payload } = DeviceManager.createDelegate({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      const privkey = manager.getIdentityPrivateKey()
      expect(privkey).toBeInstanceOf(Uint8Array)

      // Verify it corresponds to the public key
      const derivedPubkey = getPublicKey(privkey)
      expect(derivedPubkey).toBe(payload.identityPubkey)
    })
  })
})
