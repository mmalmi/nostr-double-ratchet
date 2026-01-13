import { describe, it, expect, vi, beforeEach } from "vitest"
import { DeviceManager } from "../src/DeviceManager"
import { DevicePayload } from "../src/inviteUtils"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND } from "../src/types"
import { generateSecretKey, getPublicKey, finalizeEvent } from "nostr-tools"
import { bytesToHex } from "@noble/hashes/utils"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"

describe("DeviceManager - Main Device", () => {
  let ownerPrivateKey: Uint8Array
  let ownerPublicKey: string
  let nostrSubscribe: NostrSubscribe
  let nostrPublish: NostrPublish
  let publishedEvents: any[]
  let subscriptions: Map<string, (event: any) => void>

  beforeEach(() => {
    ownerPrivateKey = generateSecretKey()
    ownerPublicKey = getPublicKey(ownerPrivateKey)
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

  describe("createMain()", () => {
    it("should create a DeviceManager in main mode", () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(DeviceManager)
    })

    it("should return isDelegateMode() === false", () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager.isDelegateMode()).toBe(false)
    })
  })

  describe("init()", () => {
    it("should create InviteList with own device", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const inviteList = manager.getInviteList()
      expect(inviteList).not.toBeNull()

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(1)
      expect(devices[0].deviceId).toBe("main-device")
      expect(devices[0].deviceLabel).toBe("Main Device")
    })

    it("should publish InviteList on init", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should have published an InviteList event
      const inviteListEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      )
      expect(inviteListEvents.length).toBeGreaterThan(0)
    })

    it("should load existing InviteList from storage", async () => {
      const storage = new InMemoryStorageAdapter()

      // Create first manager and init
      const manager1 = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager1.init()

      // Get the ephemeral key from first manager
      const ephemeralKey1 = manager1.getEphemeralKeypair()?.publicKey

      // Create second manager with same storage
      const manager2 = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager2.init()

      // Should have loaded the same InviteList (same ephemeral key)
      const ephemeralKey2 = manager2.getEphemeralKeypair()?.publicKey
      expect(ephemeralKey2).toBe(ephemeralKey1)
    })

    it("should merge local and remote InviteLists", async () => {
      const storage = new InMemoryStorageAdapter()

      // Create and init first manager
      const manager1 = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "device-1",
        deviceLabel: "Device 1",
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager1.init()

      // Simulate a remote InviteList with a different device
      const remoteDeviceId = "device-2"
      const remoteEphemeralPrivkey = generateSecretKey()
      const remoteEphemeralPubkey = getPublicKey(remoteEphemeralPrivkey)
      const remoteSharedSecret = bytesToHex(generateSecretKey())

      // Create a properly signed remote event
      const unsignedRemoteEvent = {
        kind: INVITE_LIST_EVENT_KIND,
        pubkey: ownerPublicKey,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
          ["d", "double-ratchet/invite-list"],
          [
            "device",
            remoteEphemeralPubkey,
            remoteSharedSecret,
            remoteDeviceId,
            "Device 2",
            String(Math.floor(Date.now() / 1000)),
          ],
        ],
        content: "",
      }
      const signedRemoteEvent = finalizeEvent(unsignedRemoteEvent, ownerPrivateKey)

      // Create a second manager that will "fetch" the remote list
      const manager2 = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "device-1",
        deviceLabel: "Device 1",
        nostrSubscribe: vi.fn((filter, onEvent) => {
          // Simulate returning a remote InviteList with device-2
          if (filter.kinds?.includes(INVITE_LIST_EVENT_KIND)) {
            setTimeout(() => {
              onEvent(signedRemoteEvent)
            }, 10)
          }
          return () => {}
        }) as unknown as NostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager2.init()

      // Should have both devices after merge
      const devices = manager2.getOwnDevices()
      const deviceIds = devices.map((d) => d.deviceId)
      expect(deviceIds).toContain("device-1")
      expect(deviceIds).toContain("device-2")
    })
  })

  describe("addDevice()", () => {
    it("should add device to InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const payload: DevicePayload = {
        ephemeralPubkey: getPublicKey(generateSecretKey()),
        sharedSecret: bytesToHex(generateSecretKey()),
        deviceId: "secondary-device",
        deviceLabel: "Secondary Device",
      }

      await manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(2)
      const secondaryDevice = devices.find((d) => d.deviceId === "secondary-device")
      expect(secondaryDevice).toBeDefined()
      expect(secondaryDevice?.deviceLabel).toBe("Secondary Device")
    })

    it("should publish updated InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const initialPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      ).length

      const payload: DevicePayload = {
        ephemeralPubkey: getPublicKey(generateSecretKey()),
        sharedSecret: bytesToHex(generateSecretKey()),
        deviceId: "secondary-device",
        deviceLabel: "Secondary Device",
      }

      await manager.addDevice(payload)

      const finalPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      ).length
      expect(finalPublishCount).toBeGreaterThan(initialPublishCount)
    })

    it("should include identityPubkey for delegate devices", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const delegateIdentityPubkey = getPublicKey(generateSecretKey())
      const payload: DevicePayload = {
        ephemeralPubkey: getPublicKey(generateSecretKey()),
        sharedSecret: bytesToHex(generateSecretKey()),
        deviceId: "delegate-device",
        deviceLabel: "Delegate Device",
        identityPubkey: delegateIdentityPubkey,
      }

      await manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      const delegateDevice = devices.find((d) => d.deviceId === "delegate-device")
      expect(delegateDevice?.identityPubkey).toBe(delegateIdentityPubkey)
    })
  })

  describe("revokeDevice()", () => {
    it("should remove device from InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      // Add a device first
      const payload: DevicePayload = {
        ephemeralPubkey: getPublicKey(generateSecretKey()),
        sharedSecret: bytesToHex(generateSecretKey()),
        deviceId: "secondary-device",
        deviceLabel: "Secondary Device",
      }
      await manager.addDevice(payload)

      expect(manager.getOwnDevices().length).toBe(2)

      // Revoke the device
      await manager.revokeDevice("secondary-device")

      expect(manager.getOwnDevices().length).toBe(1)
      expect(manager.getOwnDevices()[0].deviceId).toBe("main-device")
    })

    it("should publish updated InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      // Add a device first
      const payload: DevicePayload = {
        ephemeralPubkey: getPublicKey(generateSecretKey()),
        sharedSecret: bytesToHex(generateSecretKey()),
        deviceId: "secondary-device",
        deviceLabel: "Secondary Device",
      }
      await manager.addDevice(payload)

      const publishCountAfterAdd = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      ).length

      // Revoke the device
      await manager.revokeDevice("secondary-device")

      const publishCountAfterRevoke = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      ).length
      expect(publishCountAfterRevoke).toBeGreaterThan(publishCountAfterAdd)
    })

    it("should not allow revoking own device", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      await expect(manager.revokeDevice("main-device")).rejects.toThrow()
    })
  })

  describe("updateDeviceLabel()", () => {
    it("should update device label in InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      await manager.updateDeviceLabel("main-device", "Updated Label")

      const devices = manager.getOwnDevices()
      expect(devices[0].deviceLabel).toBe("Updated Label")
    })

    it("should publish updated InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const initialPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      ).length

      await manager.updateDeviceLabel("main-device", "Updated Label")

      const finalPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND
      ).length
      expect(finalPublishCount).toBeGreaterThan(initialPublishCount)
    })
  })

  describe("getOwnDevices()", () => {
    it("should return all devices from InviteList", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      // Add two more devices
      for (let i = 1; i <= 2; i++) {
        await manager.addDevice({
          ephemeralPubkey: getPublicKey(generateSecretKey()),
          sharedSecret: bytesToHex(generateSecretKey()),
          deviceId: `device-${i}`,
          deviceLabel: `Device ${i}`,
        })
      }

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(3)
    })
  })

  describe("getters", () => {
    let manager: DeviceManager

    beforeEach(async () => {
      manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()
    })

    it("getIdentityPublicKey() should return owner pubkey", () => {
      expect(manager.getIdentityPublicKey()).toBe(ownerPublicKey)
    })

    it("getIdentityPrivateKey() should return owner privkey", () => {
      expect(manager.getIdentityPrivateKey()).toEqual(ownerPrivateKey)
    })

    it("getDeviceId() should return device ID", () => {
      expect(manager.getDeviceId()).toBe("main-device")
    })

    it("getEphemeralKeypair() should return ephemeral keys", () => {
      const keypair = manager.getEphemeralKeypair()
      expect(keypair).not.toBeNull()
      expect(keypair?.publicKey).toHaveLength(64) // hex pubkey
      expect(keypair?.privateKey).toBeInstanceOf(Uint8Array)
    })

    it("getSharedSecret() should return shared secret", () => {
      const secret = manager.getSharedSecret()
      expect(secret).not.toBeNull()
      expect(secret).toHaveLength(64) // hex string
    })

    it("getInviteList() should return InviteList", () => {
      const inviteList = manager.getInviteList()
      expect(inviteList).not.toBeNull()
    })
  })

  describe("close()", () => {
    it("should clean up subscriptions", async () => {
      const manager = DeviceManager.createMain({
        ownerPublicKey,
        ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      // Should have some subscriptions active
      expect(subscriptions.size).toBeGreaterThan(0)

      manager.close()

      // Subscriptions should be cleaned up
      // (implementation detail - manager should call unsubscribe functions)
    })
  })
})
