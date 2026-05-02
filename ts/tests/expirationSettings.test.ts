import { describe, expect, it } from "vitest"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { ExpirationSettings } from "../src/session-manager/expirationSettings"

describe("session-manager ExpirationSettings", () => {
  it("persists and reloads default, peer, and group expiration policy", async () => {
    const storage = new InMemoryStorageAdapter()
    const settings = new ExpirationSettings(storage, "v1")

    await settings.setDefault({ ttlSeconds: 60 })
    await settings.setPeer("peer-a", null)
    await settings.setPeer("peer-b", { expiresAt: 1_700_000_000 })
    await settings.setGroup("group / with spaces", { ttlSeconds: 30 })

    const reloaded = new ExpirationSettings(storage, "v1")
    await reloaded.load()

    expect(reloaded.default).toEqual({ ttlSeconds: 60 })
    expect(reloaded.hasPeer("peer-a")).toBe(true)
    expect(reloaded.peer("peer-a")).toBeNull()
    expect(reloaded.peer("peer-b")).toEqual({ expiresAt: 1_700_000_000 })
    expect(reloaded.hasGroup("group / with spaces")).toBe(true)
    expect(reloaded.group("group / with spaces")).toEqual({ ttlSeconds: 30 })
  })

  it("clears overrides and rejects conflicting expiration options", async () => {
    const storage = new InMemoryStorageAdapter()
    const settings = new ExpirationSettings(storage, "v1")

    await settings.setDefault({ ttlSeconds: 60 })
    await settings.setPeer("peer-a", { ttlSeconds: 10 })
    await settings.setGroup("group-a", null)

    await settings.setDefault(undefined)
    await settings.setPeer("peer-a", undefined)
    await settings.setGroup("group-a", undefined)

    const reloaded = new ExpirationSettings(storage, "v1")
    await reloaded.load()

    expect(reloaded.default).toBeUndefined()
    expect(reloaded.hasPeer("peer-a")).toBe(false)
    expect(reloaded.hasGroup("group-a")).toBe(false)
    await expect(
      settings.setDefault({ ttlSeconds: 1, expiresAt: 2 })
    ).rejects.toThrow()
  })
})
