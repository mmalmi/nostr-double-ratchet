import { describe, it, expect, vi, beforeEach } from "vitest"
import { DeviceManager } from "../src/DeviceManager"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND } from "../src/types"
import { generateSecretKey, getPublicKey, finalizeEvent } from "nostr-tools"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { InviteList } from "../src/InviteList"

describe("DeviceManager Integration", () => {
  // Shared state for simulating relay - must be module-level for sharing
  let publishedEvents: any[]
  let subscribers: Array<{ filter: any; callback: (event: any) => void }>
  // Map of pubkey -> private key for signing
  let signingKeys: Map<string, Uint8Array>

  const matchesFilter = (event: any, filter: any): boolean => {
    if (filter.kinds && !filter.kinds.includes(event.kind)) return false
    if (filter.authors && !filter.authors.includes(event.pubkey)) return false
    if (filter["#d"]) {
      const dTag = event.tags.find((t: string[]) => t[0] === "d")?.[1]
      if (!filter["#d"].includes(dTag)) return false
    }
    if (filter["#p"]) {
      const pTags = event.tags
        .filter((t: string[]) => t[0] === "p")
        .map((t: string[]) => t[1])
      if (!filter["#p"].some((p: string) => pTags.includes(p))) return false
    }
    return true
  }

  // Factory that creates subscribe/publish functions sharing the same state
  const createNostrSubscribe = (): NostrSubscribe => {
    return vi.fn((filter, onEvent) => {
      const sub = { filter, callback: onEvent }
      subscribers.push(sub)

      // Replay matching published events
      for (const event of publishedEvents) {
        if (matchesFilter(event, filter)) {
          setTimeout(() => onEvent(event), 5)
        }
      }

      return () => {
        const index = subscribers.indexOf(sub)
        if (index > -1) subscribers.splice(index, 1)
      }
    }) as unknown as NostrSubscribe
  }

  const createNostrPublish = (): NostrPublish => {
    return vi.fn(async (event) => {
      // Sign the event if we have the private key
      let signedEvent = event
      if (!event.sig && event.pubkey && signingKeys.has(event.pubkey)) {
        const privkey = signingKeys.get(event.pubkey)!
        signedEvent = finalizeEvent(event, privkey)
      }

      publishedEvents.push(signedEvent)

      // Broadcast to all matching subscribers
      for (const sub of subscribers) {
        if (matchesFilter(signedEvent, sub.filter)) {
          setTimeout(() => sub.callback(signedEvent), 5)
        }
      }

      return signedEvent
    }) as unknown as NostrPublish
  }

  // Helper to register signing keys
  const registerSigningKey = (pubkey: string, privkey: Uint8Array) => {
    signingKeys.set(pubkey, privkey)
  }

  beforeEach(() => {
    publishedEvents = []
    subscribers = []
    signingKeys = new Map()
  })

  it("main device adds delegate, delegate activates", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    // Register signing key so publish can sign events
    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // 1. Create delegate device manager (generates keys, returns payload)
    const { manager: delegateManager, payload } = DeviceManager.createDelegate({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    // 2. Initialize delegate FIRST so it's subscribed before main publishes
    await delegateManager.init()
    const activationPromise = delegateManager.waitForActivation(5000)

    // 3. Create and init main device manager
    const mainManager = DeviceManager.createMain({
      ownerPublicKey,
      ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await mainManager.init()

    // 4. Main adds delegate using the payload
    await mainManager.addDevice(payload)

    // Verify delegate was added
    const devices = mainManager.getOwnDevices()
    expect(devices.length).toBe(2)
    expect(devices.some((d) => d.deviceId === "phone-123")).toBe(true)

    // 5. Wait for event propagation and activation
    await new Promise((resolve) => setTimeout(resolve, 50))

    const activatedOwnerPubkey = await activationPromise

    // 6. Verify
    expect(activatedOwnerPubkey).toBe(ownerPublicKey)
    expect(delegateManager.getOwnerPublicKey()).toBe(ownerPublicKey)
  })

  it("main device revokes delegate, delegate detects revocation", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    // Register signing key
    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // Setup: create and activate delegate
    const { manager: delegateManager, payload } = DeviceManager.createDelegate({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    // Initialize delegate first
    await delegateManager.init()
    const activationPromise = delegateManager.waitForActivation(5000)

    const mainManager = DeviceManager.createMain({
      ownerPublicKey,
      ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await mainManager.init()
    await mainManager.addDevice(payload)

    await new Promise((resolve) => setTimeout(resolve, 50))
    await activationPromise

    // Verify not revoked initially
    const initialRevoked = await delegateManager.isRevoked()
    expect(initialRevoked).toBe(false)

    // Main revokes delegate
    await mainManager.revokeDevice("phone-123")

    // Wait for propagation
    await new Promise((resolve) => setTimeout(resolve, 50))

    // Delegate checks revocation
    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })

  it("multiple delegates can be added and activated independently", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    // Register signing key
    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // Create two delegate devices first
    const { manager: delegate1, payload: payload1 } = DeviceManager.createDelegate({
      deviceId: "phone-1",
      deviceLabel: "Phone 1",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    const { manager: delegate2, payload: payload2 } = DeviceManager.createDelegate({
      deviceId: "phone-2",
      deviceLabel: "Phone 2",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    // Initialize delegates and start waiting for activation BEFORE main publishes
    await delegate1.init()
    await delegate2.init()

    const activation1 = delegate1.waitForActivation(5000)
    const activation2 = delegate2.waitForActivation(5000)

    // Create main manager
    const mainManager = DeviceManager.createMain({
      ownerPublicKey,
      ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await mainManager.init()

    // Main adds both delegates
    await mainManager.addDevice(payload1)
    await mainManager.addDevice(payload2)

    // Verify both were added
    const devices = mainManager.getOwnDevices()
    expect(devices.length).toBe(3) // main + 2 delegates
    expect(devices.some((d) => d.deviceId === "phone-1")).toBe(true)
    expect(devices.some((d) => d.deviceId === "phone-2")).toBe(true)

    // Wait for propagation
    await new Promise((resolve) => setTimeout(resolve, 50))

    const owner1 = await activation1
    const owner2 = await activation2

    expect(owner1).toBe(ownerPublicKey)
    expect(owner2).toBe(ownerPublicKey)
  })

  it("delegate cannot activate if not added to InviteList", async () => {
    const { manager: delegateManager } = DeviceManager.createDelegate({
      deviceId: "phone-orphan",
      deviceLabel: "Orphan Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await delegateManager.init()

    // Should timeout since no one added this delegate
    await expect(delegateManager.waitForActivation(200)).rejects.toThrow(
      "Activation timeout"
    )
  })

  it("delegate payload contains all necessary info for main device", () => {
    const { payload } = DeviceManager.createDelegate({
      deviceId: "test-device",
      deviceLabel: "Test Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    // Payload should have everything main device needs
    expect(payload.deviceId).toBe("test-device")
    expect(payload.deviceLabel).toBe("Test Device")
    expect(payload.ephemeralPubkey).toHaveLength(64)
    expect(payload.sharedSecret).toHaveLength(64)
    expect(payload.identityPubkey).toHaveLength(64)

    // These are valid hex strings
    expect(() => BigInt("0x" + payload.ephemeralPubkey)).not.toThrow()
    expect(() => BigInt("0x" + payload.sharedSecret)).not.toThrow()
    expect(() => BigInt("0x" + payload.identityPubkey)).not.toThrow()
  })

  it("main device can discover delegate identity from InviteList", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    // Register signing key
    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const { manager: delegateManager, payload } = DeviceManager.createDelegate({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    const mainManager = DeviceManager.createMain({
      ownerPublicKey,
      ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    await mainManager.init()
    await mainManager.addDevice(payload)

    // Get the delegate device entry from InviteList
    const inviteList = mainManager.getInviteList()
    const delegateEntry = inviteList?.getDevice("phone-123")

    expect(delegateEntry).toBeDefined()
    expect(delegateEntry?.identityPubkey).toBe(payload.identityPubkey)
    expect(delegateEntry?.identityPubkey).toBe(delegateManager.getIdentityPublicKey())
  })

  it("external user can discover delegate via owner's InviteList", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    // Register signing key
    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // Main device adds delegate
    const { payload } = DeviceManager.createDelegate({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    const mainManager = DeviceManager.createMain({
      ownerPublicKey,
      ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    await mainManager.init()
    await mainManager.addDevice(payload)

    // Wait for events to be published
    await new Promise((resolve) => setTimeout(resolve, 50))

    // External user fetches owner's InviteList
    // In Nostr, you'd get the latest event by created_at, so simulate by getting all and taking latest
    const externalSubscribe = createNostrSubscribe()

    const fetchedList = await new Promise<InviteList | null>((resolve) => {
      let latestEvent: any = null
      const unsub = externalSubscribe(
        {
          kinds: [INVITE_LIST_EVENT_KIND],
          authors: [ownerPublicKey],
          "#d": ["double-ratchet/invite-list"],
        },
        (event) => {
          try {
            // Keep track of the latest event by created_at
            if (!latestEvent || event.created_at >= latestEvent.created_at) {
              latestEvent = event
            }
          } catch {
            // ignore
          }
        }
      )

      setTimeout(() => {
        unsub()
        if (latestEvent) {
          try {
            resolve(InviteList.fromEvent(latestEvent))
          } catch {
            resolve(null)
          }
        } else {
          resolve(null)
        }
      }, 100)
    })

    expect(fetchedList).not.toBeNull()

    const devices = fetchedList!.getAllDevices()
    expect(devices.length).toBe(2) // main + delegate

    const delegateDevice = devices.find((d) => d.deviceId === "phone-123")
    expect(delegateDevice).toBeDefined()
    expect(delegateDevice?.identityPubkey).toBe(payload.identityPubkey)
  })
})
