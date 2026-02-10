import { describe, it, expect } from "vitest"
import { MessageQueue } from "../src/MessageQueue"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { Rumor } from "../src/types"

const makeRumor = (id: string, content = "test"): Rumor => ({
  id,
  pubkey: "sender",
  created_at: Math.floor(Date.now() / 1000),
  kind: 14,
  tags: [],
  content,
})

describe("MessageQueue", () => {
  const createQueue = (prefix = "v1/test-queue/") => {
    const storage = new InMemoryStorageAdapter()
    const queue = new MessageQueue(storage, prefix)
    return { queue, storage }
  }

  describe("add", () => {
    it("should store an entry and return a unique id", async () => {
      const { queue } = createQueue()
      const event = makeRumor("evt1")
      const id = await queue.add("device-a", event)
      expect(id).toBeTruthy()
      expect(typeof id).toBe("string")
    })

    it("should generate different ids for each add", async () => {
      const { queue } = createQueue()
      const event = makeRumor("evt1")
      const id1 = await queue.add("device-a", event)
      const id2 = await queue.add("device-a", event)
      expect(id1).not.toBe(id2)
    })
  })

  describe("getForTarget", () => {
    it("should return empty array when no entries exist", async () => {
      const { queue } = createQueue()
      const entries = await queue.getForTarget("device-a")
      expect(entries).toEqual([])
    })

    it("should return entries only for the requested target", async () => {
      const { queue } = createQueue()
      await queue.add("device-a", makeRumor("evt1", "for-a"))
      await queue.add("device-b", makeRumor("evt2", "for-b"))
      await queue.add("device-a", makeRumor("evt3", "for-a-2"))

      const entriesA = await queue.getForTarget("device-a")
      expect(entriesA).toHaveLength(2)
      expect(entriesA.map((e) => e.event.content)).toEqual(["for-a", "for-a-2"])

      const entriesB = await queue.getForTarget("device-b")
      expect(entriesB).toHaveLength(1)
      expect(entriesB[0].event.content).toBe("for-b")
    })

    it("should return entries sorted by createdAt", async () => {
      const { queue, storage } = createQueue()

      // Manually insert with controlled timestamps
      await storage.put("v1/test-queue/z-late", {
        id: "z-late",
        targetKey: "device-a",
        event: makeRumor("evt2", "second"),
        createdAt: 2000,
      })
      await storage.put("v1/test-queue/a-early", {
        id: "a-early",
        targetKey: "device-a",
        event: makeRumor("evt1", "first"),
        createdAt: 1000,
      })

      const entries = await queue.getForTarget("device-a")
      expect(entries).toHaveLength(2)
      expect(entries[0].event.content).toBe("first")
      expect(entries[1].event.content).toBe("second")
    })

    it("should deduplicate entries with the same event id", async () => {
      const { queue } = createQueue()
      const event = makeRumor("same-event-id", "hello")
      await queue.add("device-a", event)
      await queue.add("device-a", event)
      await queue.add("device-a", event)

      const entries = await queue.getForTarget("device-a")
      expect(entries).toHaveLength(1)
      expect(entries[0].event.content).toBe("hello")
    })

    it("should not deduplicate across different targets", async () => {
      const { queue } = createQueue()
      const event = makeRumor("shared-evt", "hello")
      await queue.add("device-a", event)
      await queue.add("device-b", event)

      const a = await queue.getForTarget("device-a")
      const b = await queue.getForTarget("device-b")
      expect(a).toHaveLength(1)
      expect(b).toHaveLength(1)
    })
  })

  describe("removeForTarget", () => {
    it("should remove all entries for a target", async () => {
      const { queue } = createQueue()
      await queue.add("device-a", makeRumor("evt1"))
      await queue.add("device-a", makeRumor("evt2"))
      await queue.add("device-b", makeRumor("evt3"))

      await queue.removeForTarget("device-a")

      expect(await queue.getForTarget("device-a")).toEqual([])
      expect(await queue.getForTarget("device-b")).toHaveLength(1)
    })

    it("should be a no-op when target has no entries", async () => {
      const { queue } = createQueue()
      await queue.add("device-a", makeRumor("evt1"))
      await queue.removeForTarget("device-nonexistent")
      expect(await queue.getForTarget("device-a")).toHaveLength(1)
    })

    it("should remove duplicate storage entries for same event id", async () => {
      const { queue, storage } = createQueue()
      const event = makeRumor("dup-evt")
      await queue.add("device-a", event)
      await queue.add("device-a", event)

      // getForTarget deduplicates, but storage has 2 entries
      const keysBefore = await storage.list("v1/test-queue/")
      const deviceAKeysBefore: string[] = []
      for (const k of keysBefore) {
        const entry = await storage.get<{ targetKey: string }>(k)
        if (entry?.targetKey === "device-a") deviceAKeysBefore.push(k)
      }
      expect(deviceAKeysBefore).toHaveLength(2)

      await queue.removeForTarget("device-a")

      // Both storage entries should be gone
      const keysAfter = await storage.list("v1/test-queue/")
      expect(keysAfter).toHaveLength(0)
    })
  })

  describe("remove", () => {
    it("should remove a single entry by id", async () => {
      const { queue } = createQueue()
      const id1 = await queue.add("device-a", makeRumor("evt1", "first"))
      await queue.add("device-a", makeRumor("evt2", "second"))

      await queue.remove(id1)

      const entries = await queue.getForTarget("device-a")
      expect(entries).toHaveLength(1)
      expect(entries[0].event.content).toBe("second")
    })

    it("should be a no-op for nonexistent id", async () => {
      const { queue } = createQueue()
      await queue.add("device-a", makeRumor("evt1"))
      await queue.remove("nonexistent-id")
      expect(await queue.getForTarget("device-a")).toHaveLength(1)
    })
  })

  describe("prefix isolation", () => {
    it("two queues with different prefixes on same storage should not interfere", async () => {
      const storage = new InMemoryStorageAdapter()
      const queueA = new MessageQueue(storage, "v1/message-queue/")
      const queueB = new MessageQueue(storage, "v1/discovery-queue/")

      await queueA.add("target-1", makeRumor("evt1", "from-A"))
      await queueB.add("target-1", makeRumor("evt2", "from-B"))

      const entriesA = await queueA.getForTarget("target-1")
      const entriesB = await queueB.getForTarget("target-1")

      expect(entriesA).toHaveLength(1)
      expect(entriesA[0].event.content).toBe("from-A")
      expect(entriesB).toHaveLength(1)
      expect(entriesB[0].event.content).toBe("from-B")
    })

    it("removeForTarget on one queue should not affect the other", async () => {
      const storage = new InMemoryStorageAdapter()
      const queueA = new MessageQueue(storage, "v1/message-queue/")
      const queueB = new MessageQueue(storage, "v1/discovery-queue/")

      await queueA.add("target-1", makeRumor("evt1"))
      await queueB.add("target-1", makeRumor("evt2"))

      await queueA.removeForTarget("target-1")

      expect(await queueA.getForTarget("target-1")).toEqual([])
      expect(await queueB.getForTarget("target-1")).toHaveLength(1)
    })
  })
})
