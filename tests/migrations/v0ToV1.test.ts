import { describe, it, expect, beforeEach } from "vitest"
import { v0ToV1 } from "../../src/migrations/v0ToV1"
import { MigrationContext } from "../../src/migrations/runner"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { Invite } from "../../src/Invite"
import { generateSecretKey, getPublicKey } from "nostr-tools"

describe("v0ToV1 migration", () => {
  let storage: InMemoryStorageAdapter
  let ctx: MigrationContext

  beforeEach(() => {
    storage = new InMemoryStorageAdapter()
    ctx = {
      storage,
      deviceId: "test-device",
      ourPublicKey: getPublicKey(generateSecretKey()),
      nostrSubscribe: () => () => {},
      nostrPublish: async () => {},
    }
  })

  describe("metadata", () => {
    it("should have correct version info", () => {
      expect(v0ToV1.name).toBe("v0ToV1")
      expect(v0ToV1.fromVersion).toBe(null)
      expect(v0ToV1.toVersion).toBe("1")
    })
  })

  describe("invite migration", () => {
    it("should move invites from invite/{pubkey} to v1/invite/{pubkey}", async () => {
      const pubkey = getPublicKey(generateSecretKey())
      const invite = Invite.createNew(pubkey, "device-1")

      // Store in old location
      await storage.put(`invite/${pubkey}`, invite.serialize())

      await v0ToV1.migrate(ctx)

      // Old key should be gone
      expect(await storage.get(`invite/${pubkey}`)).toBeUndefined()
      // New key should exist
      expect(await storage.get(`v1/invite/${pubkey}`)).toBeDefined()
    })

    it("should preserve invite data after migration", async () => {
      const pubkey = getPublicKey(generateSecretKey())
      const invite = Invite.createNew(pubkey, "device-1")

      await storage.put(`invite/${pubkey}`, invite.serialize())

      await v0ToV1.migrate(ctx)

      const migratedData = await storage.get<string>(`v1/invite/${pubkey}`)
      const migratedInvite = Invite.deserialize(migratedData!)

      expect(migratedInvite.inviterEphemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
      expect(migratedInvite.sharedSecret).toBe(invite.sharedSecret)
      expect(migratedInvite.deviceId).toBe(invite.deviceId)
    })

    it("should migrate multiple invites", async () => {
      const pubkey1 = getPublicKey(generateSecretKey())
      const pubkey2 = getPublicKey(generateSecretKey())

      await storage.put(`invite/${pubkey1}`, Invite.createNew(pubkey1, "d1").serialize())
      await storage.put(`invite/${pubkey2}`, Invite.createNew(pubkey2, "d2").serialize())

      await v0ToV1.migrate(ctx)

      expect(await storage.get(`v1/invite/${pubkey1}`)).toBeDefined()
      expect(await storage.get(`v1/invite/${pubkey2}`)).toBeDefined()
      expect(await storage.get(`invite/${pubkey1}`)).toBeUndefined()
      expect(await storage.get(`invite/${pubkey2}`)).toBeUndefined()
    })

    it("should handle no invites to migrate", async () => {
      // No invites stored
      await v0ToV1.migrate(ctx)

      const keys = await storage.list("v1/invite/")
      expect(keys).toHaveLength(0)
    })
  })

  describe("user record migration", () => {
    it("should move user records from user/{pubkey} to v1/user/{pubkey}", async () => {
      const pubkey = getPublicKey(generateSecretKey())
      const userRecord = {
        publicKey: pubkey,
        devices: [{ deviceId: "d1", createdAt: 1000 }],
      }

      await storage.put(`user/${pubkey}`, userRecord)

      await v0ToV1.migrate(ctx)

      expect(await storage.get(`user/${pubkey}`)).toBeUndefined()
      expect(await storage.get(`v1/user/${pubkey}`)).toBeDefined()
    })

    it("should clear sessions from user records", async () => {
      const pubkey = getPublicKey(generateSecretKey())
      const userRecord = {
        publicKey: pubkey,
        devices: [{
          deviceId: "d1",
          createdAt: 1000,
          activeSession: { some: "session data" },
          inactiveSessions: [{ old: "session" }],
        }],
      }

      await storage.put(`user/${pubkey}`, userRecord)

      await v0ToV1.migrate(ctx)

      const migrated = await storage.get<any>(`v1/user/${pubkey}`)
      expect(migrated.devices[0].activeSession).toBeNull()
      expect(migrated.devices[0].inactiveSessions).toEqual([])
    })

    it("should preserve device metadata", async () => {
      const pubkey = getPublicKey(generateSecretKey())
      const userRecord = {
        publicKey: pubkey,
        devices: [{
          deviceId: "my-device",
          createdAt: 12345,
        }],
      }

      await storage.put(`user/${pubkey}`, userRecord)

      await v0ToV1.migrate(ctx)

      const migrated = await storage.get<any>(`v1/user/${pubkey}`)
      expect(migrated.publicKey).toBe(pubkey)
      expect(migrated.devices[0].deviceId).toBe("my-device")
      expect(migrated.devices[0].createdAt).toBe(12345)
    })

    it("should migrate multiple user records", async () => {
      const pubkey1 = getPublicKey(generateSecretKey())
      const pubkey2 = getPublicKey(generateSecretKey())

      await storage.put(`user/${pubkey1}`, { publicKey: pubkey1, devices: [] })
      await storage.put(`user/${pubkey2}`, { publicKey: pubkey2, devices: [] })

      await v0ToV1.migrate(ctx)

      expect(await storage.get(`v1/user/${pubkey1}`)).toBeDefined()
      expect(await storage.get(`v1/user/${pubkey2}`)).toBeDefined()
    })
  })

  describe("error handling", () => {
    it("should continue migrating other items if one fails", async () => {
      const goodPubkey = getPublicKey(generateSecretKey())

      // Store a valid invite
      await storage.put(`invite/${goodPubkey}`, Invite.createNew(goodPubkey, "d1").serialize())
      // Store invalid data that will fail deserialization
      await storage.put(`invite/bad`, "not valid json")

      await v0ToV1.migrate(ctx)

      // Good invite should still be migrated
      expect(await storage.get(`v1/invite/${goodPubkey}`)).toBeDefined()
    })
  })
})
