import { describe, it, expect, beforeEach, vi } from "vitest"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { UnsignedEvent, VerifiedEvent } from "nostr-tools"
import { generateSecretKey, getPublicKey } from "nostr-tools"

describe("ControlledMockRelay", () => {
  let relay: ControlledMockRelay
  let secretKey: Uint8Array
  let pubkey: string

  beforeEach(() => {
    relay = new ControlledMockRelay()
    secretKey = generateSecretKey()
    pubkey = getPublicKey(secretKey)
  })

  function createEvent(kind: number, content: string): UnsignedEvent {
    return {
      kind,
      content,
      pubkey,
      created_at: Math.floor(Date.now() / 1000),
      tags: [],
    }
  }

  describe("Publishing", () => {
    it("should queue events in pending without delivering", async () => {
      const received: VerifiedEvent[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e))

      await relay.publish(createEvent(1, "test"), secretKey)

      expect(received).toHaveLength(0)
      expect(relay.getPendingCount()).toBe(1)
    })

    it("should deliver immediately with publishAndDeliver", async () => {
      const received: VerifiedEvent[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e))

      await relay.publishAndDeliver(createEvent(1, "test"), secretKey)

      expect(received).toHaveLength(1)
      expect(relay.getPendingCount()).toBe(0)
    })
  })

  describe("Delivery Control", () => {
    it("deliverNext should deliver in FIFO order", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "first"), secretKey)
      await relay.publish(createEvent(1, "second"), secretKey)
      await relay.publish(createEvent(1, "third"), secretKey)

      relay.deliverNext()
      expect(received).toEqual(["first"])

      relay.deliverNext()
      expect(received).toEqual(["first", "second"])
    })

    it("deliverAll should deliver all pending events", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "first"), secretKey)
      await relay.publish(createEvent(1, "second"), secretKey)
      await relay.publish(createEvent(1, "third"), secretKey)

      relay.deliverAll()
      expect(received).toEqual(["first", "second", "third"])
      expect(relay.getPendingCount()).toBe(0)
    })

    it("deliverEvent should deliver specific event by ID", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "first"), secretKey)
      const id2 = await relay.publish(createEvent(1, "second"), secretKey)
      await relay.publish(createEvent(1, "third"), secretKey)

      relay.deliverEvent(id2)
      expect(received).toEqual(["second"])
      expect(relay.getPendingCount()).toBe(2)
    })

    it("deliverInOrder should deliver events in specified order", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      const id1 = await relay.publish(createEvent(1, "first"), secretKey)
      const id2 = await relay.publish(createEvent(1, "second"), secretKey)
      const id3 = await relay.publish(createEvent(1, "third"), secretKey)

      // Deliver in reverse order
      relay.deliverInOrder([id3, id1, id2])
      expect(received).toEqual(["third", "first", "second"])
    })

    it("deliverTo should deliver to specific subscriber only", async () => {
      const alice: string[] = []
      const bob: string[] = []

      const subA = relay.subscribe([{ kinds: [1] }], (e) => alice.push(e.content))
      const subB = relay.subscribe([{ kinds: [1] }], (e) => bob.push(e.content))

      const id = await relay.publish(createEvent(1, "message"), secretKey)

      relay.deliverTo(subA.id, id)
      expect(alice).toEqual(["message"])
      expect(bob).toEqual([])

      // Bob can still receive it later
      relay.deliverTo(subB.id, id)
      expect(bob).toEqual(["message"])
    })

    it("deliverAllTo should deliver all pending to specific subscriber", async () => {
      const alice: string[] = []
      const bob: string[] = []

      const subA = relay.subscribe([{ kinds: [1] }], (e) => alice.push(e.content))
      relay.subscribe([{ kinds: [1] }], (e) => bob.push(e.content))

      await relay.publish(createEvent(1, "first"), secretKey)
      await relay.publish(createEvent(1, "second"), secretKey)

      relay.deliverAllTo(subA.id)
      expect(alice).toEqual(["first", "second"])
      expect(bob).toEqual([])
    })
  })

  describe("Timing Control", () => {
    it("deliverNextAfter should delay delivery", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "delayed"), secretKey)

      const start = Date.now()
      await relay.deliverNextAfter(50)
      const elapsed = Date.now() - start

      expect(elapsed).toBeGreaterThanOrEqual(45)
      expect(received).toEqual(["delayed"])
    })

    it("deliverAllWithDelay should space out deliveries", async () => {
      const timestamps: number[] = []
      relay.subscribe([{ kinds: [1] }], () => timestamps.push(Date.now()))

      await relay.publish(createEvent(1, "first"), secretKey)
      await relay.publish(createEvent(1, "second"), secretKey)
      await relay.publish(createEvent(1, "third"), secretKey)

      await relay.deliverAllWithDelay(30)

      expect(timestamps).toHaveLength(3)
      expect(timestamps[1] - timestamps[0]).toBeGreaterThanOrEqual(25)
      expect(timestamps[2] - timestamps[1]).toBeGreaterThanOrEqual(25)
    })
  })

  describe("Failure Injection", () => {
    it("dropEvent should remove event without delivering", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      const id = await relay.publish(createEvent(1, "will be dropped"), secretKey)
      await relay.publish(createEvent(1, "will be delivered"), secretKey)

      relay.dropEvent(id)
      expect(relay.getPendingCount()).toBe(1)

      relay.deliverAll()
      expect(received).toEqual(["will be delivered"])
    })

    it("dropNext should drop N events from front of queue", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "first"), secretKey)
      await relay.publish(createEvent(1, "second"), secretKey)
      await relay.publish(createEvent(1, "third"), secretKey)

      relay.dropNext(2)
      relay.deliverAll()
      expect(received).toEqual(["third"])
    })

    it("duplicateEvent should deliver same event twice", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      const id = await relay.publish(createEvent(1, "dup me"), secretKey)
      relay.deliverEvent(id)
      relay.duplicateEvent(id)

      expect(received).toEqual(["dup me", "dup me"])
      expect(relay.getDeliveryCount(id)).toBe(2)
    })

    it("simulateDisconnect should close all subscriptions", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "before disconnect"), secretKey)
      relay.simulateDisconnect()

      // Events published after disconnect go to pending but no subscribers
      relay.deliverAll()
      expect(received).toEqual([])
      expect(relay.getSubscriptions()).toHaveLength(0)
    })

    it("simulateReconnect should replay events to new subscriptions", async () => {
      // Publish and deliver some events
      await relay.publishAndDeliver(createEvent(1, "stored1"), secretKey)
      await relay.publishAndDeliver(createEvent(1, "stored2"), secretKey)

      // Disconnect
      relay.simulateDisconnect()

      // Reconnect with new subscription
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      relay.simulateReconnect()
      expect(received).toEqual(["stored1", "stored2"])
    })
  })

  describe("EOSE Control", () => {
    it("sendEose should call onEose callback", async () => {
      const eoseCallback = vi.fn()
      const sub = relay.subscribe([{ kinds: [1] }], () => {}, eoseCallback)

      relay.sendEose(sub.id)
      expect(eoseCallback).toHaveBeenCalledTimes(1)
    })

    it("sendEose should only fire once per subscription", async () => {
      const eoseCallback = vi.fn()
      const sub = relay.subscribe([{ kinds: [1] }], () => {}, eoseCallback)

      relay.sendEose(sub.id)
      relay.sendEose(sub.id)
      expect(eoseCallback).toHaveBeenCalledTimes(1)
    })

    it("sendEoseToAll should send EOSE to all subscriptions", async () => {
      const eose1 = vi.fn()
      const eose2 = vi.fn()

      relay.subscribe([{ kinds: [1] }], () => {}, eose1)
      relay.subscribe([{ kinds: [2] }], () => {}, eose2)

      relay.sendEoseToAll()
      expect(eose1).toHaveBeenCalledTimes(1)
      expect(eose2).toHaveBeenCalledTimes(1)
    })

    it("autoEose should send EOSE after delivering stored events", async () => {
      // Store some events first
      await relay.publishAndDeliver(createEvent(1, "stored"), secretKey)

      relay.setAutoEose(true)

      const received: string[] = []
      const eoseCallback = vi.fn()

      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content), eoseCallback)

      // Need to wait for queueMicrotask
      await new Promise<void>((resolve) => queueMicrotask(() => resolve()))

      expect(received).toEqual(["stored"])
      expect(eoseCallback).toHaveBeenCalledTimes(1)
    })
  })

  describe("Filter Matching", () => {
    it("should only deliver events matching subscription filters", async () => {
      const kind1Events: string[] = []
      const kind2Events: string[] = []

      relay.subscribe([{ kinds: [1] }], (e) => kind1Events.push(e.content))
      relay.subscribe([{ kinds: [2] }], (e) => kind2Events.push(e.content))

      await relay.publish(createEvent(1, "kind1"), secretKey)
      await relay.publish(createEvent(2, "kind2"), secretKey)
      await relay.publish(createEvent(1, "kind1 again"), secretKey)

      relay.deliverAll()

      expect(kind1Events).toEqual(["kind1", "kind1 again"])
      expect(kind2Events).toEqual(["kind2"])
    })

    it("should support multiple filters per subscription", async () => {
      const received: string[] = []

      relay.subscribe(
        [{ kinds: [1] }, { kinds: [2] }],
        (e) => received.push(e.content)
      )

      await relay.publish(createEvent(1, "kind1"), secretKey)
      await relay.publish(createEvent(2, "kind2"), secretKey)
      await relay.publish(createEvent(3, "kind3"), secretKey)

      relay.deliverAll()

      expect(received).toEqual(["kind1", "kind2"])
    })
  })

  describe("Inspection", () => {
    it("getPendingEvents should return pending events", async () => {
      await relay.publish(createEvent(1, "pending1"), secretKey)
      await relay.publish(createEvent(1, "pending2"), secretKey)

      const pending = relay.getPendingEvents()
      expect(pending).toHaveLength(2)
      expect(pending[0].content).toBe("pending1")
    })

    it("getAllEvents should return both pending and delivered", async () => {
      await relay.publishAndDeliver(createEvent(1, "delivered"), secretKey)
      await relay.publish(createEvent(1, "pending"), secretKey)

      const all = relay.getAllEvents()
      expect(all).toHaveLength(2)
    })

    it("wasDeliveredTo should track delivery history", async () => {
      const sub = relay.subscribe([{ kinds: [1] }], () => {})
      const id = await relay.publish(createEvent(1, "test"), secretKey)

      expect(relay.wasDeliveredTo(id, sub.id)).toBe(false)

      relay.deliverAll()
      expect(relay.wasDeliveredTo(id, sub.id)).toBe(true)
    })

    it("getDeliveryHistory should return full history", async () => {
      const sub = relay.subscribe([{ kinds: [1] }], () => {})
      await relay.publish(createEvent(1, "test"), secretKey)

      relay.deliverAll()

      const history = relay.getDeliveryHistory()
      expect(history).toHaveLength(1)
      expect(history[0].subscriberId).toBe(sub.id)
    })

    it("getSubscriptions should return active subscriptions", async () => {
      const sub1 = relay.subscribe([{ kinds: [1] }], () => {})
      relay.subscribe([{ kinds: [2] }], () => {})

      expect(relay.getSubscriptions()).toHaveLength(2)

      sub1.close()
      expect(relay.getSubscriptions()).toHaveLength(1)
    })
  })

  describe("Reset", () => {
    it("reset should clear all state", async () => {
      relay.subscribe([{ kinds: [1] }], () => {})
      await relay.publish(createEvent(1, "test"), secretKey)
      relay.deliverAll()

      relay.reset()

      expect(relay.getPendingCount()).toBe(0)
      expect(relay.getAllEvents()).toHaveLength(0)
      expect(relay.getSubscriptions()).toHaveLength(0)
      expect(relay.getDeliveryHistory()).toHaveLength(0)
    })

    it("clearPending should only clear pending events", async () => {
      relay.subscribe([{ kinds: [1] }], () => {})
      await relay.publishAndDeliver(createEvent(1, "delivered"), secretKey)
      await relay.publish(createEvent(1, "pending"), secretKey)

      relay.clearPending()

      expect(relay.getPendingCount()).toBe(0)
      expect(relay.getAllEvents()).toHaveLength(1) // delivered event remains
    })

    it("clearHistory should only clear delivery history", async () => {
      relay.subscribe([{ kinds: [1] }], () => {})
      await relay.publish(createEvent(1, "test"), secretKey)
      relay.deliverAll()

      expect(relay.getDeliveryHistory()).toHaveLength(1)

      relay.clearHistory()
      expect(relay.getDeliveryHistory()).toHaveLength(0)
      expect(relay.getAllEvents()).toHaveLength(1) // events remain
    })
  })

  describe("Subscription Close", () => {
    it("closed subscriptions should not receive events", async () => {
      const received: string[] = []
      const sub = relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      await relay.publish(createEvent(1, "before close"), secretKey)
      sub.close()
      await relay.publish(createEvent(1, "after close"), secretKey)

      relay.deliverAll()
      expect(received).toEqual([])
    })
  })
})
