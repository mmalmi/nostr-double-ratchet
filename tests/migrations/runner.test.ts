import { describe, it, expect, beforeEach, vi } from "vitest"
import { runMigrations, Migration, MigrationContext } from "../../src/migrations/runner"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { generateSecretKey, getPublicKey } from "nostr-tools"

describe("runMigrations", () => {
  let storage: InMemoryStorageAdapter
  let ctx: MigrationContext
  let migrationCalls: string[]

  beforeEach(() => {
    storage = new InMemoryStorageAdapter()
    migrationCalls = []

    ctx = {
      storage,
      deviceId: "test-device",
      ourPublicKey: getPublicKey(generateSecretKey()),
      nostrSubscribe: () => () => {},
      nostrPublish: async () => {},
    }
  })

  function createMigration(name: string, from: string | null, to: string): Migration {
    return {
      name,
      fromVersion: from,
      toVersion: to,
      migrate: async () => {
        migrationCalls.push(name)
      },
    }
  }

  describe("version tracking", () => {
    it("should start with no version", async () => {
      const version = await storage.get<string>("storage-version")
      expect(version).toBeUndefined()
    })

    it("should set version after running migration", async () => {
      const migrations = [createMigration("m1", null, "1")]

      await runMigrations(ctx, migrations)

      expect(await storage.get<string>("storage-version")).toBe("1")
    })

    it("should update version after each migration", async () => {
      const migrations = [
        createMigration("m1", null, "1"),
        createMigration("m2", "1", "2"),
      ]

      await runMigrations(ctx, migrations)

      expect(await storage.get<string>("storage-version")).toBe("2")
    })
  })

  describe("migration selection", () => {
    it("should run migration when fromVersion is null and no version set", async () => {
      const migrations = [createMigration("m1", null, "1")]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual(["m1"])
    })

    it("should run migration when fromVersion matches current version", async () => {
      await storage.put("storage-version", "1")
      const migrations = [createMigration("m2", "1", "2")]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual(["m2"])
    })

    it("should skip migration when fromVersion does not match", async () => {
      await storage.put("storage-version", "2")
      const migrations = [createMigration("m1", "1", "2")]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual([])
    })

    it("should not run null->1 migration if version is already set", async () => {
      await storage.put("storage-version", "1")
      const migrations = [createMigration("m1", null, "1")]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual([])
    })
  })

  describe("sequential execution", () => {
    it("should run migrations in order", async () => {
      const migrations = [
        createMigration("m1", null, "1"),
        createMigration("m2", "1", "2"),
        createMigration("m3", "2", "3"),
      ]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual(["m1", "m2", "m3"])
      expect(await storage.get<string>("storage-version")).toBe("3")
    })

    it("should continue from current version", async () => {
      await storage.put("storage-version", "1")
      const migrations = [
        createMigration("m1", null, "1"),
        createMigration("m2", "1", "2"),
        createMigration("m3", "2", "3"),
      ]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual(["m2", "m3"])
      expect(await storage.get<string>("storage-version")).toBe("3")
    })

    it("should stop if no matching migration", async () => {
      await storage.put("storage-version", "5")
      const migrations = [
        createMigration("m1", null, "1"),
        createMigration("m2", "1", "2"),
      ]

      await runMigrations(ctx, migrations)

      expect(migrationCalls).toEqual([])
      expect(await storage.get<string>("storage-version")).toBe("5")
    })
  })

  describe("error handling", () => {
    it("should propagate migration errors", async () => {
      const failingMigration: Migration = {
        name: "failing",
        fromVersion: null,
        toVersion: "1",
        migrate: async () => {
          throw new Error("Migration failed")
        },
      }

      await expect(runMigrations(ctx, [failingMigration])).rejects.toThrow("Migration failed")
    })

    it("should not update version if migration fails", async () => {
      const failingMigration: Migration = {
        name: "failing",
        fromVersion: null,
        toVersion: "1",
        migrate: async () => {
          throw new Error("Migration failed")
        },
      }

      try {
        await runMigrations(ctx, [failingMigration])
      } catch {
        // Expected
      }

      expect(await storage.get<string>("storage-version")).toBeUndefined()
    })
  })

  describe("empty migrations", () => {
    it("should handle empty migrations array", async () => {
      await runMigrations(ctx, [])

      expect(await storage.get<string>("storage-version")).toBeUndefined()
    })
  })
})
