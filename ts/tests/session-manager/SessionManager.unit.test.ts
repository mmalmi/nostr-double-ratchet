import { describe, expect, it, vi } from "vitest"

import { InMemoryStorageAdapter, type StorageAdapter } from "../../src/StorageAdapter"
import { SessionManager } from "../../src/session-manager/SessionManager"
import type { UserSetupStatus } from "../../src/session-manager/types"

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

class DelayedUserRecordPutStorage implements StorageAdapter {
  private readonly inner = new InMemoryStorageAdapter()

  constructor(private readonly delayMs: number) {}

  async get<T = unknown>(key: string): Promise<T | undefined> {
    return this.inner.get<T>(key)
  }

  async put<T = unknown>(key: string, value: T): Promise<void> {
    if (key.startsWith("v1/user/")) {
      await sleep(this.delayMs)
    }
    await this.inner.put(key, value)
  }

  async del(key: string): Promise<void> {
    await this.inner.del(key)
  }

  async list(prefix = ""): Promise<string[]> {
    return this.inner.list(prefix)
  }
}

const createManager = (storage: StorageAdapter = new InMemoryStorageAdapter()) =>
  new SessionManager(
    "our-device-pubkey",
    new Uint8Array([1, 2, 3]),
    "our-device-id",
    vi.fn(() => vi.fn()),
    vi.fn(async (event) => ({ ...event, id: "published-id", sig: "sig" })),
    "our-owner-pubkey",
    {
      ephemeralKeypair: {
        publicKey: "ephemeral-public-key",
        privateKey: new Uint8Array([4, 5, 6]),
      },
      sharedSecret: "shared-secret",
    },
    storage
  )

describe("SessionManager unit", () => {
  it("onUserSetupStatus emits immediate current status and supports unsubscribe", () => {
    const manager = createManager()
    const seen: UserSetupStatus[] = []

    const unsubscribe = manager.onUserSetupStatus("peer-owner-pubkey", (status) => {
      seen.push(status)
    })

    expect(seen).toEqual([
      {
        ownerPublicKey: "peer-owner-pubkey",
        state: "new",
        ready: false,
        appKeysKnown: false,
      },
    ])

    unsubscribe()
    ;(manager as never).emitUserSetupStatus("peer-owner-pubkey")
    expect(seen).toHaveLength(1)
  })

  it("resolves delegate pubkeys to owner in setup status helpers", () => {
    const manager = createManager()
    ;(manager as never).delegateToOwner.set("peer-device-pubkey", "peer-owner-pubkey")
    ;(manager as never).userRecords.set("peer-owner-pubkey", {
      state: "ready",
      appKeys: { getAllDevices: () => [] },
      devices: new Map(),
    })

    const status = manager.getUserSetupStatus("peer-device-pubkey")

    expect(status.ownerPublicKey).toBe("peer-owner-pubkey")
    expect(status.state).toBe("ready")
    expect(status.appKeysKnown).toBe(true)
    expect(manager.isUserReady("peer-device-pubkey")).toBe(true)
  })

  it("startUserSetup returns latest status after ensureSetup", async () => {
    const manager = createManager()
    const fakeUserRecord = {
      state: "new",
      appKeys: undefined,
      devices: new Map(),
      ensureSetup: vi.fn(async () => {
        fakeUserRecord.state = "ready"
        fakeUserRecord.appKeys = { getAllDevices: () => [] }
      }),
    }
    ;(manager as never).userRecords.set("peer-owner-pubkey", fakeUserRecord)
    vi.spyOn(manager, "init").mockResolvedValue(undefined)

    const seenStates: string[] = []
    manager.onUserSetupStatus("peer-owner-pubkey", (status) => {
      seenStates.push(status.state)
    })

    const status = await manager.startUserSetup("peer-owner-pubkey")

    expect(fakeUserRecord.ensureSetup).toHaveBeenCalledTimes(1)
    expect(status).toEqual({
      ownerPublicKey: "peer-owner-pubkey",
      state: "ready",
      ready: true,
      appKeysKnown: true,
    })
    expect(seenStates).toContain("ready")
  })

  it("deleteChat stays durable when delayed user-record puts complete late", async () => {
    const storage = new DelayedUserRecordPutStorage(60)
    const manager = createManager(storage)
    vi.spyOn(manager, "init").mockResolvedValue(undefined)

    const ownerPubkey = "peer-owner-pubkey"
    const fakeUserRecord = {
      close: vi.fn(),
      state: "ready",
      appKeys: undefined,
      devices: new Map([
        [
          "peer-device-id",
          {
            deviceId: "peer-device-id",
            activeSession: null,
            inactiveSessions: [],
            createdAt: 1,
            revoke: vi.fn(async () => {
              manager.persistUserRecord(ownerPubkey)
            }),
          },
        ],
      ]),
    }

    ;(manager as never).userRecords.set(ownerPubkey, fakeUserRecord)
    manager.persistUserRecord(ownerPubkey)

    await manager.deleteChat(ownerPubkey)
    await sleep(180)

    expect(await storage.get(`v1/user/${ownerPubkey}`)).toBeUndefined()
    expect((manager as never).userRecords.has(ownerPubkey)).toBe(false)
  })
})
