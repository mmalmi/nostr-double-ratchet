import { describe, it, expect, beforeEach } from "vitest"
import { ControlledMockRelay } from "./helpers/ControlledMockRelay"
import { UnsignedEvent, VerifiedEvent } from "nostr-tools"
import { generateSecretKey, getPublicKey } from "nostr-tools"

/**
 * Essential tests for ControlledMockRelay test helper.
 * These verify the core functionality needed for controlled delivery testing.
 */
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

    it("deliverTo should deliver to specific subscriber only", async () => {
      const alice: string[] = []
      const bob: string[] = []

      const subA = relay.subscribe([{ kinds: [1] }], (e) => alice.push(e.content))
      const subB = relay.subscribe([{ kinds: [1] }], (e) => bob.push(e.content))

      const id = await relay.publish(createEvent(1, "message"), secretKey)

      relay.deliverTo(subA.id, id)
      expect(alice).toEqual(["message"])
      expect(bob).toEqual([])

      relay.deliverTo(subB.id, id)
      expect(bob).toEqual(["message"])
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

    it("duplicateEvent should deliver same event twice", async () => {
      const received: string[] = []
      relay.subscribe([{ kinds: [1] }], (e) => received.push(e.content))

      const id = await relay.publish(createEvent(1, "dup me"), secretKey)
      relay.deliverEvent(id)
      relay.duplicateEvent(id)

      expect(received).toEqual(["dup me", "dup me"])
      expect(relay.getDeliveryCount(id)).toBe(2)
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
  })

  describe("Inspection", () => {
    it("should track delivery history", async () => {
      const sub = relay.subscribe([{ kinds: [1] }], () => {})
      const id = await relay.publish(createEvent(1, "test"), secretKey)

      expect(relay.wasDeliveredTo(id, sub.id)).toBe(false)

      relay.deliverAll()
      expect(relay.wasDeliveredTo(id, sub.id)).toBe(true)
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
