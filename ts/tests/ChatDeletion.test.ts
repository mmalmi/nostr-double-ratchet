import { describe, it, expect } from "vitest"
import { createMockSessionManager } from "./helpers/mockSessionManager"
import { MockRelay } from "./helpers/mockRelay"
import { InMemoryStorageAdapter, StorageAdapter } from "../src/StorageAdapter"

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

describe("SessionManager local chat deletion", () => {
  it("deleteChat should remove local session state and allow explicit reinit", async () => {
    const sharedRelay = new MockRelay()
    const alice = await createMockSessionManager("alice-device-1", sharedRelay)
    const bob = await createMockSessionManager("bob-device-1", sharedRelay)

    const seed = "before-delete-init"
    const seedReceived = new Promise<void>((resolve) => {
      const unsub = bob.manager.onEvent((event) => {
        if (event.content !== seed) return
        unsub()
        resolve()
      })
    })
    await alice.manager.sendMessage(bob.publicKey, seed)
    await seedReceived

    await alice.manager.deleteChat(bob.publicKey)

    const text = "after-delete-reinit"
    const bobReceived = new Promise<void>((resolve) => {
      const unsub = bob.manager.onEvent((event) => {
        if (event.content !== text) return
        unsub()
        resolve()
      })
    })
    await alice.manager.sendMessage(bob.publicKey, text)
    await bobReceived
  })

  it("deleteChat should not be undone by delayed user-record writes", async () => {
    const sharedRelay = new MockRelay()
    const storage = new DelayedUserRecordPutStorage(60)
    const alice = await createMockSessionManager(
      "alice-device-1",
      sharedRelay,
      undefined,
      storage
    )
    const bob = await createMockSessionManager("bob-device-1", sharedRelay)

    const seed = "prime-for-delete"
    const seedReceived = new Promise<void>((resolve) => {
      const unsub = bob.manager.onEvent((event) => {
        if (event.content !== seed) return
        unsub()
        resolve()
      })
    })
    await alice.manager.sendMessage(bob.publicKey, seed)
    await seedReceived

    // Let initial delayed persistence settle before triggering deletion.
    await sleep(80)
    await alice.manager.deleteChat(bob.publicKey)

    // Wait for late writes to complete; key must still be absent.
    await sleep(160)
    expect(await storage.get(`v1/user/${bob.publicKey}`)).toBeUndefined()

    alice.manager.close()
    const aliceRestart = await createMockSessionManager(
      "alice-device-1",
      sharedRelay,
      alice.secretKey,
      storage
    )
    const internal = aliceRestart.manager as {
      userRecords?: Map<string, unknown>
    }
    expect(internal.userRecords?.has(bob.publicKey)).toBe(false)
  })
})
