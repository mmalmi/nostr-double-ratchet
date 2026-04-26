import { describe, expect, it, vi } from "vitest"
import {
  finalizeEvent,
  generateSecretKey,
  getPublicKey,
  type UnsignedEvent,
  type VerifiedEvent,
} from "nostr-tools"
import { AppKeys } from "../src/AppKeys"
import { NdrRuntime } from "../src/NdrRuntime"
import { InMemoryStorageAdapter, type StorageAdapter } from "../src/StorageAdapter"
import type { NostrPublish, NostrSubscribe } from "../src/types"
import { MockRelay } from "./helpers/mockRelay"

const tick = async (ms = 0) =>
  new Promise((resolve) => {
    setTimeout(resolve, ms)
  })

const createSubscribe = (relay: MockRelay): NostrSubscribe => {
  return (filter, onEvent) => relay.subscribe(filter, onEvent).close
}

const createRuntime = (options: {
  relay: MockRelay
  ownerPrivateKey: Uint8Array
  storage?: StorageAdapter
  appKeysDelayMs?: number
}) => {
  const { relay, ownerPrivateKey, storage, appKeysDelayMs = 0 } = options
  const publish = (async (event: UnsignedEvent | VerifiedEvent) => {
    if ("sig" in event && event.sig) {
      relay.storeAndDeliver(event as VerifiedEvent)
      return event as VerifiedEvent
    }

    const signedEvent = finalizeEvent(event, ownerPrivateKey) as VerifiedEvent
    if (appKeysDelayMs > 0) {
      setTimeout(() => {
        relay.storeAndDeliver(signedEvent)
      }, appKeysDelayMs)
    } else {
      relay.storeAndDeliver(signedEvent)
    }
    return signedEvent
  }) as NostrPublish

  return new NdrRuntime({
    nostrSubscribe: createSubscribe(relay),
    nostrPublish: publish,
    storage,
    appKeysFastTimeoutMs: 25,
    appKeysFetchTimeoutMs: 50,
  })
}

describe("NdrRuntime", () => {
  it("registers a first device without requiring relay confirmation", async () => {
    const relay = new MockRelay()
    const ownerPrivateKey = generateSecretKey()
    const ownerPubkey = getPublicKey(ownerPrivateKey)
    const runtime = createRuntime({ relay, ownerPrivateKey })

    await runtime.initForOwner(ownerPubkey)

    const result = await runtime.registerCurrentDevice({ ownerPubkey })

    expect(result.relayConfirmationRequired).toBe(false)
    expect(runtime.getState().ownerPubkey).toBe(ownerPubkey)
    expect(runtime.getState().sessionManagerReady).toBe(true)
    expect(runtime.getState().isCurrentDeviceRegistered).toBe(true)
    expect(runtime.getState().registeredDevices).toHaveLength(1)

    const relaySnapshot = await AppKeys.waitFor(ownerPubkey, createSubscribe(relay), 25)
    expect(relaySnapshot?.getAllDevices()).toHaveLength(1)
    expect(relaySnapshot?.getAllDevices()[0]?.identityPubkey).toBe(
      runtime.getState().currentDevicePubkey
    )
  })

  it("waits for relay-visible AppKeys when adding an additional device", async () => {
    const relay = new MockRelay()
    const ownerPrivateKey = generateSecretKey()
    const ownerPubkey = getPublicKey(ownerPrivateKey)

    const primaryRuntime = createRuntime({ relay, ownerPrivateKey })
    await primaryRuntime.initForOwner(ownerPubkey)
    await primaryRuntime.registerCurrentDevice({ ownerPubkey })

    const linkedRuntime = createRuntime({
      relay,
      ownerPrivateKey,
      appKeysDelayMs: 50,
    })
    await linkedRuntime.initForOwner(ownerPubkey)

    let resolved = false
    const registrationPromise = linkedRuntime
      .registerCurrentDevice({ ownerPubkey, timeoutMs: 500 })
      .then((result) => {
        resolved = true
        return result
      })

    await tick(20)
    expect(resolved).toBe(false)

    const result = await registrationPromise

    expect(result.relayConfirmationRequired).toBe(true)
    expect(linkedRuntime.getState().isCurrentDeviceRegistered).toBe(true)
    expect(linkedRuntime.getState().registeredDevices).toHaveLength(2)

    const relaySnapshot = await AppKeys.waitFor(ownerPubkey, createSubscribe(relay), 25)
    expect(relaySnapshot?.getAllDevices()).toHaveLength(2)
  })

  it("ignores stale AppKeys snapshots on the runtime subscription", async () => {
    const relay = new MockRelay()
    const ownerPrivateKey = generateSecretKey()
    const ownerPubkey = getPublicKey(ownerPrivateKey)
    const runtime = createRuntime({ relay, ownerPrivateKey })

    await runtime.initAppKeysManager()
    runtime.startAppKeysSubscription(ownerPubkey)

    const latestAppKeys = new AppKeys([
      { identityPubkey: "device-a", createdAt: 100 },
    ])
    const latestEvent = latestAppKeys.getEvent()
    latestEvent.created_at = 200
    relay.storeAndDeliver(finalizeEvent(latestEvent, ownerPrivateKey) as VerifiedEvent)
    await tick()

    expect(runtime.getState().registeredDevices).toEqual([
      { identityPubkey: "device-a", createdAt: 100 },
    ])
    expect(runtime.getState().lastAppKeysCreatedAt).toBe(200)

    const staleAppKeys = new AppKeys([])
    const staleEvent = staleAppKeys.getEvent()
    staleEvent.created_at = 150
    relay.storeAndDeliver(finalizeEvent(staleEvent, ownerPrivateKey) as VerifiedEvent)
    await tick()

    expect(runtime.getState().registeredDevices).toEqual([
      { identityPubkey: "device-a", createdAt: 100 },
    ])
    expect(runtime.getState().lastAppKeysCreatedAt).toBe(200)
  })

  it("restores the same delegate identity and local AppKeys state after restart", async () => {
    const relay = new MockRelay()
    const ownerPrivateKey = generateSecretKey()
    const ownerPubkey = getPublicKey(ownerPrivateKey)
    const storage = new InMemoryStorageAdapter()

    const firstRuntime = createRuntime({
      relay,
      ownerPrivateKey,
      storage,
    })
    await firstRuntime.initForOwner(ownerPubkey)
    await firstRuntime.registerCurrentDevice({ ownerPubkey })

    const firstDevicePubkey = firstRuntime.getState().currentDevicePubkey
    firstRuntime.close()

    const restartedRuntime = createRuntime({
      relay,
      ownerPrivateKey,
      storage,
    })
    await restartedRuntime.initForOwner(ownerPubkey)

    expect(restartedRuntime.getState().currentDevicePubkey).toBe(firstDevicePubkey)
    expect(restartedRuntime.getState().registeredDevices).toHaveLength(1)
    expect(restartedRuntime.getState().hasLocalAppKeys).toBe(true)
  })

  it("routes direct messages through runtime-owned subscriptions", async () => {
    const relay = new MockRelay()

    const aliceOwnerPrivateKey = generateSecretKey()
    const aliceOwnerPubkey = getPublicKey(aliceOwnerPrivateKey)
    const aliceRuntime = createRuntime({
      relay,
      ownerPrivateKey: aliceOwnerPrivateKey,
    })
    await aliceRuntime.initForOwner(aliceOwnerPubkey)
    await aliceRuntime.registerCurrentDevice({ ownerPubkey: aliceOwnerPubkey })
    await aliceRuntime.republishInvite()

    const bobOwnerPrivateKey = generateSecretKey()
    const bobOwnerPubkey = getPublicKey(bobOwnerPrivateKey)
    const bobRuntime = createRuntime({
      relay,
      ownerPrivateKey: bobOwnerPrivateKey,
    })
    await bobRuntime.initForOwner(bobOwnerPubkey)
    await bobRuntime.registerCurrentDevice({ ownerPubkey: bobOwnerPubkey })
    await bobRuntime.republishInvite()

    const bobReceived = new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("bob did not receive alice message")), 5_000)
      const unsubscribe = bobRuntime.onSessionEvent((event) => {
        if (event.content !== "hello via runtime") return
        clearTimeout(timeout)
        unsubscribe()
        resolve()
      })
    })

    await aliceRuntime.sendMessage(bobOwnerPubkey, "hello via runtime")
    await bobReceived

    expect(bobRuntime.getDirectMessageSubscriptionAuthors().length).toBeGreaterThan(0)

    const aliceReceived = new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("alice did not receive bob reply")), 5_000)
      const unsubscribe = aliceRuntime.onSessionEvent((event) => {
        if (event.content !== "reply via runtime") return
        clearTimeout(timeout)
        unsubscribe()
        resolve()
      })
    })

    await bobRuntime.sendMessage(aliceOwnerPubkey, "reply via runtime")
    await aliceReceived
  })

  it("owns group transport alongside sessions on the high-level runtime path", async () => {
    const relay = new MockRelay()

    const aliceOwnerPrivateKey = generateSecretKey()
    const aliceOwnerPubkey = getPublicKey(aliceOwnerPrivateKey)
    const aliceRuntime = createRuntime({
      relay,
      ownerPrivateKey: aliceOwnerPrivateKey,
    })
    await aliceRuntime.initForOwner(aliceOwnerPubkey)
    await aliceRuntime.registerCurrentDevice({ ownerPubkey: aliceOwnerPubkey })

    const bobOwnerPrivateKey = generateSecretKey()
    const bobOwnerPubkey = getPublicKey(bobOwnerPrivateKey)
    const bobRuntime = createRuntime({
      relay,
      ownerPrivateKey: bobOwnerPrivateKey,
    })
    await bobRuntime.initForOwner(bobOwnerPubkey)
    await bobRuntime.registerCurrentDevice({ ownerPubkey: bobOwnerPubkey })

    await aliceRuntime.waitForSessionManager(aliceOwnerPubkey).then((manager) => {
      return manager.setupUser(bobOwnerPubkey)
    })
    await bobRuntime.waitForSessionManager(bobOwnerPubkey).then((manager) => {
      return manager.setupUser(aliceOwnerPubkey)
    })

    const created = await aliceRuntime.createGroup("Runtime Group", [bobOwnerPubkey], {
      fanoutMetadata: false,
    })
    await bobRuntime.syncGroups([created.group], bobOwnerPubkey)
    const sent = await aliceRuntime.sendGroupMessage(created.group.id, "hello group")
    await tick()

    expect(aliceRuntime.getState().groupManagerReady).toBe(true)
    expect(bobRuntime.getState().groupManagerReady).toBe(true)
    expect(aliceRuntime.getGroupManager()?.managedGroupIds()).toContain(created.group.id)
    expect(bobRuntime.getGroupManager()?.managedGroupIds()).toContain(created.group.id)
    expect(sent.inner.content).toBe("hello group")
    expect(sent.inner.kind).toBe(14)
    expect(sent.inner.tags).toContainEqual(["l", created.group.id])
    expect(aliceRuntime.getGroupManager()?.knownSenderEventPubkeys().length).toBeGreaterThan(0)

    await bobRuntime.syncGroups([], bobOwnerPubkey)
    expect(bobRuntime.getGroupManager()?.managedGroupIds()).not.toContain(created.group.id)
    expect(relay.getAllEvents().length).toBeGreaterThanOrEqual(2)
  })

  it("exposes direct-message helper wrappers on the runtime surface", async () => {
    const relay = new MockRelay()
    const ownerPrivateKey = generateSecretKey()
    const ownerPubkey = getPublicKey(ownerPrivateKey)
    const runtime = createRuntime({
      relay,
      ownerPrivateKey,
    })
    await runtime.initForOwner(ownerPubkey)

    const manager = await runtime.waitForSessionManager(ownerPubkey)
    const sendEventRumor = {
      id: "reaction-id",
      pubkey: ownerPubkey,
      kind: 7,
      content: "🔥",
      created_at: 1,
      tags: [["e", "message-id"]],
    }
    const sendMessageRumor = {
      id: "message-id",
      pubkey: ownerPubkey,
      kind: 14,
      content: "hello runtime",
      created_at: 1,
      tags: [["p", "peer"]],
    }
    const sendTypingRumor = {
      id: "typing-id",
      pubkey: ownerPubkey,
      kind: 25,
      content: "typing",
      created_at: 1,
      tags: [["p", "peer"]],
    }
    const sendReceiptRumor = {
      id: "receipt-id",
      pubkey: ownerPubkey,
      kind: 15,
      content: "seen",
      created_at: 1,
      tags: [["e", "message-id"]],
    }

    const setupUserSpy = vi.spyOn(manager, "setupUser").mockResolvedValue()
    const sendEventSpy = vi
      .spyOn(manager, "sendEvent")
      .mockResolvedValue(sendEventRumor)
    const sendMessageSpy = vi
      .spyOn(manager, "sendMessage")
      .mockResolvedValue(sendMessageRumor)
    const sendTypingSpy = vi
      .spyOn(manager, "sendTyping")
      .mockResolvedValue(sendTypingRumor)
    const sendReceiptSpy = vi
      .spyOn(manager, "sendReceipt")
      .mockResolvedValue(sendReceiptRumor)

    await runtime.setupUser("peer")
    expect(setupUserSpy).toHaveBeenCalledWith("peer")

    await expect(runtime.sendEvent("peer", sendEventRumor)).resolves.toBe(
      sendEventRumor,
    )
    expect(sendEventSpy).toHaveBeenCalledWith("peer", sendEventRumor)

    await expect(runtime.sendMessage("peer", "hello runtime")).resolves.toBe(
      sendMessageRumor,
    )
    expect(sendMessageSpy).toHaveBeenCalledWith("peer", "hello runtime", {})

    await expect(runtime.sendTyping("peer")).resolves.toBe(sendTypingRumor)
    expect(sendTypingSpy).toHaveBeenCalledWith("peer")

    await expect(
      runtime.sendReceipt("peer", "seen", ["message-id"]),
    ).resolves.toBe(sendReceiptRumor)
    expect(sendReceiptSpy).toHaveBeenCalledWith("peer", "seen", ["message-id"])
  })
})
