import { describe, it, expect, beforeEach, vi } from "vitest"
import { v1ToV2 } from "../../src/migrations/v1ToV2"
import { MigrationContext } from "../../src/migrations/runner"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { Invite } from "../../src/Invite"
import { InviteList } from "../../src/InviteList"
import { INVITE_LIST_EVENT_KIND, INVITE_EVENT_KIND } from "../../src/types"
import { generateSecretKey, getPublicKey, VerifiedEvent } from "nostr-tools"
import { MockRelay } from "../helpers/mockRelay"

describe("v1ToV2 migration", () => {
  let storage: InMemoryStorageAdapter
  let secretKey: Uint8Array
  let publicKey: string
  let mockRelay: MockRelay
  let publishedEvents: any[]
  let ctx: MigrationContext

  beforeEach(() => {
    storage = new InMemoryStorageAdapter()
    secretKey = generateSecretKey()
    publicKey = getPublicKey(secretKey)
    mockRelay = new MockRelay()
    publishedEvents = []

    ctx = {
      storage,
      deviceId: "test-device",
      ourPublicKey: publicKey,
      nostrSubscribe: (filter, onEvent) => mockRelay.subscribe(filter, onEvent),
      nostrPublish: async (event) => {
        publishedEvents.push(event)
        return mockRelay.publish(event, secretKey)
      },
    }
  })

  describe("metadata", () => {
    it("should have correct version info", () => {
      expect(v1ToV2.name).toBe("v1ToV2")
      expect(v1ToV2.fromVersion).toBe("1")
      expect(v1ToV2.toVersion).toBe("2")
    })
  })

  describe("when no v1 invite exists", () => {
    it("should do nothing if no v1 device invite exists", async () => {
      await v1ToV2.migrate(ctx)

      // No InviteList should be created
      expect(await storage.get("v2/invite-list")).toBeUndefined()
      expect(publishedEvents).toHaveLength(0)
    })
  })

  describe("when v1 invite exists", () => {
    let invite: Invite

    beforeEach(async () => {
      invite = Invite.createNew(publicKey, "test-device")
      await storage.put(`v1/device-invite/test-device`, invite.serialize())
    })

    it("should create InviteList from v1 device invite", async () => {
      await v1ToV2.migrate(ctx)

      const savedList = await storage.get<string>("v2/invite-list")
      expect(savedList).toBeDefined()

      const inviteList = InviteList.deserialize(savedList!)
      expect(inviteList.getAllDevices()).toHaveLength(1)
    })

    it("should preserve device data in InviteList", async () => {
      await v1ToV2.migrate(ctx)

      const savedList = await storage.get<string>("v2/invite-list")
      const inviteList = InviteList.deserialize(savedList!)
      const device = inviteList.getDevice("test-device")

      expect(device).toBeDefined()
      expect(device!.ephemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
      expect(device!.sharedSecret).toBe(invite.sharedSecret)
      expect(device!.deviceId).toBe("test-device")
    })

    it("should preserve private key in InviteList", async () => {
      await v1ToV2.migrate(ctx)

      const savedList = await storage.get<string>("v2/invite-list")
      const inviteList = InviteList.deserialize(savedList!)
      const device = inviteList.getDevice("test-device")

      expect(device!.ephemeralPrivateKey).toStrictEqual(invite.inviterEphemeralPrivateKey)
    })

    it("should delete old v1 device invite", async () => {
      await v1ToV2.migrate(ctx)

      expect(await storage.get(`v1/device-invite/test-device`)).toBeUndefined()
    })

    it("should publish InviteList event (kind 10078)", async () => {
      await v1ToV2.migrate(ctx)

      const inviteListEvent = publishedEvents.find(e => e.kind === INVITE_LIST_EVENT_KIND)
      expect(inviteListEvent).toBeDefined()
      expect(inviteListEvent.kind).toBe(10078)
    })

    it("should publish tombstone for old invite (kind 30078)", async () => {
      await v1ToV2.migrate(ctx)

      const tombstone = publishedEvents.find(e => e.kind === INVITE_EVENT_KIND)
      expect(tombstone).toBeDefined()
      expect(tombstone.kind).toBe(30078)

      // Tombstone should not have keys
      expect(tombstone.tags.some((t: string[]) => t[0] === "ephemeralKey")).toBe(false)
      expect(tombstone.tags.some((t: string[]) => t[0] === "sharedSecret")).toBe(false)

      // Tombstone should have d-tag
      const dTag = tombstone.tags.find((t: string[]) => t[0] === "d")
      expect(dTag).toBeDefined()
      expect(dTag[1]).toBe("double-ratchet/invites/test-device")
    })
  })

  describe("merging with existing remote InviteList", () => {
    let localInvite: Invite

    beforeEach(async () => {
      localInvite = Invite.createNew(publicKey, "local-device")
      ctx.deviceId = "local-device"
      await storage.put(`v1/device-invite/local-device`, localInvite.serialize())
    })

    it("should merge with existing InviteList from relay", async () => {
      // Another device already migrated and published an InviteList
      const remoteList = new InviteList(publicKey)
      const remoteDevice = remoteList.createDevice("Remote Device", "remote-device")
      remoteList.addDevice(remoteDevice)
      const publishedEvent = await mockRelay.publish(remoteList.getEvent(), secretKey)

      // Verify the event is on the relay and parseable
      expect(mockRelay.getEvents()).toHaveLength(1)
      expect(InviteList.fromEvent(publishedEvent).getDevice("remote-device")).toBeDefined()

      await v1ToV2.migrate(ctx)

      const savedList = await storage.get<string>("v2/invite-list")
      const inviteList = InviteList.deserialize(savedList!)

      // Should have both devices
      expect(inviteList.getAllDevices()).toHaveLength(2)
      expect(inviteList.getDevice("local-device")).toBeDefined()
      expect(inviteList.getDevice("remote-device")).toBeDefined()
    })

    it("should preserve local private key when merging", async () => {
      // Remote list has same device but no private key
      const remoteList = new InviteList(publicKey)
      const remoteDevice = {
        ephemeralPublicKey: localInvite.inviterEphemeralPublicKey,
        sharedSecret: localInvite.sharedSecret,
        deviceId: "local-device",
        deviceLabel: "Local Device",
        createdAt: localInvite.createdAt,
        // No ephemeralPrivateKey (came from event)
      }
      remoteList.addDevice(remoteDevice)
      await mockRelay.publish(remoteList.getEvent(), secretKey)

      await v1ToV2.migrate(ctx)

      const savedList = await storage.get<string>("v2/invite-list")
      const inviteList = InviteList.deserialize(savedList!)
      const device = inviteList.getDevice("local-device")

      // Private key should be preserved from local
      expect(device!.ephemeralPrivateKey).toStrictEqual(localInvite.inviterEphemeralPrivateKey)
    })
  })

  describe("error handling", () => {
    it("should handle invalid v1 invite data gracefully", async () => {
      await storage.put(`v1/device-invite/test-device`, "invalid json")

      // Should not throw
      await v1ToV2.migrate(ctx)

      // Nothing should be created
      expect(await storage.get("v2/invite-list")).toBeUndefined()
    })

    it("should handle publish failure gracefully", async () => {
      const invite = Invite.createNew(publicKey, "test-device")
      await storage.put(`v1/device-invite/test-device`, invite.serialize())

      ctx.nostrPublish = async () => {
        throw new Error("Network error")
      }

      // Should not throw
      await v1ToV2.migrate(ctx)

      // InviteList should still be saved locally
      expect(await storage.get("v2/invite-list")).toBeDefined()
    })
  })
})
