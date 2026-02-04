import { describe, it, expect, beforeEach } from "vitest"
import { MessageQueue, StoredQueueItem } from "../src/MessageQueue"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { Rumor } from "../src/types"

const createTestRumor = (id: string, content: string = "test"): Rumor => ({
  id,
  pubkey: "sender-pubkey",
  created_at: Math.floor(Date.now() / 1000),
  kind: 14,
  tags: [],
  content,
})

describe("MessageQueue", () => {
  let storage: InMemoryStorageAdapter
  let queue: MessageQueue

  beforeEach(() => {
    storage = new InMemoryStorageAdapter()
    queue = new MessageQueue({ storage, versionPrefix: "v1" })
  })

  describe("enqueue", () => {
    it("should persist a message to storage", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient-owner", ["device-1", "device-2"])

      expect(item.id).toBe("msg-1")
      expect(item.rumor).toEqual(rumor)
      expect(item.recipientOwnerPubkey).toBe("recipient-owner")
      expect(item.targetDevices).toEqual(["device-1", "device-2"])
      expect(item.queuedAt).toBeGreaterThan(0)
    })

    it("should create device status entries for all target devices", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient", ["device-1", "device-2"])

      expect(item.deviceStatus["device-1"]).toEqual({ sent: false })
      expect(item.deviceStatus["device-2"]).toEqual({ sent: false })
    })

    it("should handle empty target devices", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient", [])

      expect(item.targetDevices).toEqual([])
      expect(Object.keys(item.deviceStatus)).toHaveLength(0)
    })
  })

  describe("dequeue", () => {
    it("should remove a message from storage", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1"])

      await queue.dequeue("msg-1")

      const item = await queue.getItem("msg-1")
      expect(item).toBeUndefined()
    })

    it("should handle non-existent message gracefully", async () => {
      // Should not throw
      await queue.dequeue("non-existent")
    })
  })

  describe("getItem", () => {
    it("should return the queue item by ID", async () => {
      const rumor = createTestRumor("msg-1", "hello")
      await queue.enqueue(rumor, "recipient", ["device-1"])

      const item = await queue.getItem("msg-1")
      expect(item).toBeDefined()
      expect(item!.rumor.content).toBe("hello")
    })

    it("should return undefined for non-existent item", async () => {
      const item = await queue.getItem("non-existent")
      expect(item).toBeUndefined()
    })
  })

  describe("loadAll", () => {
    it("should return all items sorted by queuedAt", async () => {
      const rumor1 = createTestRumor("msg-1")
      const rumor2 = createTestRumor("msg-2")
      const rumor3 = createTestRumor("msg-3")

      // Enqueue with slight delays to get different queuedAt values
      await queue.enqueue(rumor2, "recipient", ["device-1"])
      await new Promise((r) => setTimeout(r, 5))
      await queue.enqueue(rumor1, "recipient", ["device-1"])
      await new Promise((r) => setTimeout(r, 5))
      await queue.enqueue(rumor3, "recipient", ["device-1"])

      const items = await queue.loadAll()

      expect(items).toHaveLength(3)
      // Items should be sorted by queuedAt (oldest first)
      expect(items[0].id).toBe("msg-2")
      expect(items[1].id).toBe("msg-1")
      expect(items[2].id).toBe("msg-3")
    })

    it("should return empty array when queue is empty", async () => {
      const items = await queue.loadAll()
      expect(items).toEqual([])
    })
  })

  describe("updateDeviceStatus", () => {
    it("should mark device as sent with timestamp", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1"])

      const beforeUpdate = Date.now()
      await queue.updateDeviceStatus("msg-1", "device-1", true)

      const item = await queue.getItem("msg-1")
      expect(item!.deviceStatus["device-1"].sent).toBe(true)
      expect(item!.deviceStatus["device-1"].sentAt).toBeGreaterThanOrEqual(beforeUpdate)
    })

    it("should mark device as not sent (clear timestamp)", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1"])
      await queue.updateDeviceStatus("msg-1", "device-1", true)

      await queue.updateDeviceStatus("msg-1", "device-1", false)

      const item = await queue.getItem("msg-1")
      expect(item!.deviceStatus["device-1"].sent).toBe(false)
      expect(item!.deviceStatus["device-1"].sentAt).toBeUndefined()
    })

    it("should handle non-existent message gracefully", async () => {
      // Should not throw
      await queue.updateDeviceStatus("non-existent", "device-1", true)
    })
  })

  describe("addDeviceToItem", () => {
    it("should add new device to target devices", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1"])

      await queue.addDeviceToItem("msg-1", "device-2")

      const item = await queue.getItem("msg-1")
      expect(item!.targetDevices).toContain("device-2")
      expect(item!.deviceStatus["device-2"]).toEqual({ sent: false })
    })

    it("should not add duplicate device", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1"])

      await queue.addDeviceToItem("msg-1", "device-1")

      const item = await queue.getItem("msg-1")
      expect(item!.targetDevices.filter((d) => d === "device-1")).toHaveLength(1)
    })

    it("should handle non-existent message gracefully", async () => {
      // Should not throw
      await queue.addDeviceToItem("non-existent", "device-1")
    })
  })

  describe("isComplete", () => {
    it("should return false when devices are pending", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient", ["device-1", "device-2"])

      expect(queue.isComplete(item)).toBe(false)
    })

    it("should return false when some devices are sent", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1", "device-2"])
      await queue.updateDeviceStatus("msg-1", "device-1", true)

      const item = (await queue.getItem("msg-1"))!
      expect(queue.isComplete(item)).toBe(false)
    })

    it("should return true when all devices are sent", async () => {
      const rumor = createTestRumor("msg-1")
      await queue.enqueue(rumor, "recipient", ["device-1", "device-2"])
      await queue.updateDeviceStatus("msg-1", "device-1", true)
      await queue.updateDeviceStatus("msg-1", "device-2", true)

      const item = (await queue.getItem("msg-1"))!
      expect(queue.isComplete(item)).toBe(true)
    })

    it("should return true for empty target devices", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient", [])

      expect(queue.isComplete(item)).toBe(true)
    })
  })

  describe("isReadyForDequeue", () => {
    it("should return false when not complete", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient", ["device-1"])

      expect(queue.isReadyForDequeue(item)).toBe(false)
    })

    it("should return false when complete but within hold time", async () => {
      const rumor = createTestRumor("msg-1")
      const item = await queue.enqueue(rumor, "recipient", ["device-1"])
      await queue.updateDeviceStatus("msg-1", "device-1", true)

      const updatedItem = (await queue.getItem("msg-1"))!
      expect(queue.isReadyForDequeue(updatedItem)).toBe(false)
    })

    it("should return true when complete and past hold time", async () => {
      const rumor = createTestRumor("msg-1")

      // Create item with old queuedAt time
      const oldItem: StoredQueueItem = {
        id: rumor.id,
        rumor,
        recipientOwnerPubkey: "recipient",
        queuedAt: Date.now() - MessageQueue.HOLD_TIME_MS - 100,
        deviceStatus: { "device-1": { sent: true, sentAt: Date.now() } },
        targetDevices: ["device-1"],
      }

      expect(queue.isReadyForDequeue(oldItem)).toBe(true)
    })

    it("should handle empty target devices with hold time", async () => {
      const rumor = createTestRumor("msg-1")

      // Create item with empty devices and old queuedAt
      const oldItem: StoredQueueItem = {
        id: rumor.id,
        rumor,
        recipientOwnerPubkey: "recipient",
        queuedAt: Date.now() - MessageQueue.HOLD_TIME_MS - 100,
        deviceStatus: {},
        targetDevices: [],
      }

      expect(queue.isReadyForDequeue(oldItem)).toBe(true)
    })
  })

  describe("getItemsForRecipient", () => {
    it("should return only items for the specified recipient", async () => {
      await queue.enqueue(createTestRumor("msg-1"), "alice", ["device-1"])
      await queue.enqueue(createTestRumor("msg-2"), "bob", ["device-1"])
      await queue.enqueue(createTestRumor("msg-3"), "alice", ["device-1"])

      const aliceItems = await queue.getItemsForRecipient("alice")
      expect(aliceItems).toHaveLength(2)
      expect(aliceItems.map((i) => i.id)).toContain("msg-1")
      expect(aliceItems.map((i) => i.id)).toContain("msg-3")
    })

    it("should return empty array when no items for recipient", async () => {
      await queue.enqueue(createTestRumor("msg-1"), "alice", ["device-1"])

      const bobItems = await queue.getItemsForRecipient("bob")
      expect(bobItems).toEqual([])
    })
  })

  describe("deleteItemsForRecipient", () => {
    it("should delete all items for the specified recipient", async () => {
      await queue.enqueue(createTestRumor("msg-1"), "alice", ["device-1"])
      await queue.enqueue(createTestRumor("msg-2"), "bob", ["device-1"])
      await queue.enqueue(createTestRumor("msg-3"), "alice", ["device-1"])

      await queue.deleteItemsForRecipient("alice")

      const allItems = await queue.loadAll()
      expect(allItems).toHaveLength(1)
      expect(allItems[0].id).toBe("msg-2")
    })

    it("should handle no items for recipient gracefully", async () => {
      await queue.enqueue(createTestRumor("msg-1"), "alice", ["device-1"])

      // Should not throw
      await queue.deleteItemsForRecipient("bob")

      const allItems = await queue.loadAll()
      expect(allItems).toHaveLength(1)
    })
  })

  describe("storage persistence", () => {
    it("should survive re-instantiation with same storage", async () => {
      // Enqueue with first instance
      const rumor = createTestRumor("msg-1", "persistent message")
      await queue.enqueue(rumor, "recipient", ["device-1"])
      await queue.updateDeviceStatus("msg-1", "device-1", true)

      // Create new instance with same storage
      const queue2 = new MessageQueue({ storage, versionPrefix: "v1" })

      const item = await queue2.getItem("msg-1")
      expect(item).toBeDefined()
      expect(item!.rumor.content).toBe("persistent message")
      expect(item!.deviceStatus["device-1"].sent).toBe(true)
    })
  })

  describe("version prefix isolation", () => {
    it("should not see items from different version prefix", async () => {
      const v1Queue = new MessageQueue({ storage, versionPrefix: "v1" })
      const v2Queue = new MessageQueue({ storage, versionPrefix: "v2" })

      await v1Queue.enqueue(createTestRumor("msg-v1"), "recipient", ["device-1"])
      await v2Queue.enqueue(createTestRumor("msg-v2"), "recipient", ["device-1"])

      const v1Items = await v1Queue.loadAll()
      const v2Items = await v2Queue.loadAll()

      expect(v1Items).toHaveLength(1)
      expect(v1Items[0].id).toBe("msg-v1")

      expect(v2Items).toHaveLength(1)
      expect(v2Items[0].id).toBe("msg-v2")
    })

    it("should not affect items in other version prefix", async () => {
      const v1Queue = new MessageQueue({ storage, versionPrefix: "v1" })
      const v2Queue = new MessageQueue({ storage, versionPrefix: "v2" })

      await v1Queue.enqueue(createTestRumor("msg-v1"), "recipient", ["device-1"])
      await v2Queue.enqueue(createTestRumor("msg-v2"), "recipient", ["device-1"])

      // Dequeue from v1
      await v1Queue.dequeue("msg-v1")

      // v2 should still have its item
      const v2Items = await v2Queue.loadAll()
      expect(v2Items).toHaveLength(1)
      expect(v2Items[0].id).toBe("msg-v2")
    })
  })
})
