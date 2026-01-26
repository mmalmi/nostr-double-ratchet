import { describe, it, expect, vi, beforeEach } from "vitest"
import { DeviceManager, DelegateManager, DelegatePayload } from "../src/DeviceManager"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, INVITE_EVENT_KIND } from "../src/types"
import { generateSecretKey, getPublicKey, finalizeEvent } from "nostr-tools"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { InviteList } from "../src/InviteList"

describe("DelegateManager", () => {
  let nostrSubscribe: NostrSubscribe
  let nostrPublish: NostrPublish
  let publishedEvents: any[]
  let subscriptions: Map<string, (event: any) => void>

  beforeEach(() => {
    publishedEvents = []
    subscriptions = new Map()

    nostrSubscribe = vi.fn((filter, onEvent) => {
      const key = JSON.stringify(filter)
      subscriptions.set(key, onEvent)
      return () => {
        subscriptions.delete(key)
      }
    }) as unknown as NostrSubscribe

    nostrPublish = vi.fn(async (event) => {
      publishedEvents.push(event)
      return event
    }) as unknown as NostrPublish
  })

  describe("create()", () => {
    it("should create a DelegateManager", () => {
      const { manager } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(DelegateManager)
    })

    it("should generate identity keypair", () => {
      const { manager, payload } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      expect(payload.identityPubkey).toBeDefined()
      expect(payload.identityPubkey).toHaveLength(64)
      expect(manager.getIdentityPublicKey()).toBe(payload.identityPubkey)

      const privkey = manager.getIdentityKey()
      expect(privkey).toBeInstanceOf(Uint8Array)
      expect((privkey as Uint8Array).length).toBe(32)
    })

    it("should return simplified payload with only identityPubkey", () => {
      const { payload } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      // Simplified payload only contains identityPubkey
      expect(payload.identityPubkey).toBeDefined()
      // No deviceId, deviceLabel, ephemeralPubkey, or sharedSecret
      expect((payload as any).deviceId).toBeUndefined()
      expect((payload as any).deviceLabel).toBeUndefined()
      expect((payload as any).ephemeralPubkey).toBeUndefined()
      expect((payload as any).sharedSecret).toBeUndefined()
    })
  })

  describe("init()", () => {
    it("should publish Invite event (not InviteList)", async () => {
      const { manager } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should NOT publish InviteList (only DeviceManager does that)
      const inviteListEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      )
      expect(inviteListEvents.length).toBe(0)

      // Should publish its own Invite event
      const inviteEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1]?.startsWith("double-ratchet/invites/"))
      )
      expect(inviteEvents.length).toBe(1)
    })

    it("should create and store Invite on init", async () => {
      const storage = new InMemoryStorageAdapter()

      const { manager } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()

      const invite = manager.getInvite()
      expect(invite).not.toBeNull()
      expect(invite?.inviterEphemeralPublicKey).toHaveLength(64)
      expect(invite?.inviterEphemeralPrivateKey).toBeInstanceOf(Uint8Array)
      expect(invite?.sharedSecret).toHaveLength(64)
    })

    it("should load stored owner pubkey if exists", async () => {
      const storage = new InMemoryStorageAdapter()
      const ownerPubkey = getPublicKey(generateSecretKey())

      await storage.put("v3/device-manager/owner-pubkey", ownerPubkey)

      const { manager } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()

      expect(manager.getOwnerPublicKey()).toBe(ownerPubkey)
    })
  })

  describe("waitForActivation()", () => {
    it("should subscribe to InviteList events", async () => {
      const { manager } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const activationPromise = manager.waitForActivation(100)

      expect(nostrSubscribe).toHaveBeenCalled()
      const calls = (nostrSubscribe as any).mock.calls
      const inviteListCall = calls.find(
        (call: any) => call[0].kinds?.includes(INVITE_LIST_EVENT_KIND)
      )
      expect(inviteListCall).toBeDefined()

      await expect(activationPromise).rejects.toThrow("Activation timeout")
    })

    it("should resolve when own identityPubkey appears in an InviteList", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const activationPromise = manager.waitForActivation(5000)

      await new Promise((resolve) => setTimeout(resolve, 50))

      // Simplified tag format: ["device", identityPubkey, createdAt]
      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["version", "3"],
            [
              "device",
              payload.identityPubkey,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      const subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        const callback = subscriptions.get(subscriptionKey)
        callback?.(inviteListEvent)
      }

      const result = await activationPromise
      expect(result).toBe(ownerPublicKey)
    })

    it("should resolve immediately if already activated", async () => {
      const storage = new InMemoryStorageAdapter()
      const ownerPubkey = getPublicKey(generateSecretKey())

      await storage.put("v3/device-manager/owner-pubkey", ownerPubkey)

      const { manager } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()

      const result = await manager.waitForActivation(100)
      expect(result).toBe(ownerPubkey)
    })
  })

  describe("isRevoked()", () => {
    it("should return false when device is in InviteList", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const activationPromise = manager.waitForActivation(5000)
      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["version", "3"],
            [
              "device",
              payload.identityPubkey,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      let subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      await activationPromise

      const isRevokedSubscribe = vi.fn((filter, onEvent) => {
        if (
          filter.kinds?.includes(INVITE_LIST_EVENT_KIND) &&
          filter.authors?.includes(ownerPublicKey)
        ) {
          setTimeout(() => onEvent(inviteListEvent), 10)
        }
        return () => {}
      }) as unknown as NostrSubscribe

      ;(manager as any).nostrSubscribe = isRevokedSubscribe

      const revoked = await manager.isRevoked()
      expect(revoked).toBe(false)
    })

    it("should return true when device is removed from InviteList", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DelegateManager.create({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const activationPromise = manager.waitForActivation(5000)
      await new Promise((resolve) => setTimeout(resolve, 50))

      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["version", "3"],
            [
              "device",
              payload.identityPubkey,
              String(Math.floor(Date.now() / 1000)),
            ],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      let subscriptionKey = Array.from(subscriptions.keys()).find((key) =>
        key.includes(String(INVITE_LIST_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.(inviteListEvent)
      }

      await activationPromise

      // Simplified removed tag format: ["removed", identityPubkey, removedAt]
      const revokedInviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000) + 1,
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["version", "3"],
            ["removed", payload.identityPubkey, String(Math.floor(Date.now() / 1000))],
          ],
          content: "",
        },
        ownerPrivateKey
      )

      const isRevokedSubscribe = vi.fn((filter, onEvent) => {
        if (
          filter.kinds?.includes(INVITE_LIST_EVENT_KIND) &&
          filter.authors?.includes(ownerPublicKey)
        ) {
          setTimeout(() => onEvent(revokedInviteListEvent), 10)
        }
        return () => {}
      }) as unknown as NostrSubscribe

      ;(manager as any).nostrSubscribe = isRevokedSubscribe

      const revoked = await manager.isRevoked()
      expect(revoked).toBe(true)
    })
  })
})

describe("DeviceManager - Authority", () => {
  let ownerPrivateKey: Uint8Array
  let ownerPublicKey: string
  let nostrSubscribe: NostrSubscribe
  let nostrPublish: NostrPublish
  let publishedEvents: any[]
  let subscriptions: Map<string, (event: any) => void>

  beforeEach(() => {
    ownerPrivateKey = generateSecretKey()
    ownerPublicKey = getPublicKey(ownerPrivateKey)
    publishedEvents = []
    subscriptions = new Map()

    nostrSubscribe = vi.fn((filter, onEvent) => {
      const key = JSON.stringify(filter)
      subscriptions.set(key, onEvent)
      return () => {
        subscriptions.delete(key)
      }
    }) as unknown as NostrSubscribe

    nostrPublish = vi.fn(async (event) => {
      publishedEvents.push(event)
      return event
    }) as unknown as NostrPublish
  })

  describe("constructor", () => {
    it("should create a DeviceManager", () => {
      const manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(DeviceManager)
    })
  })

  describe("init()", () => {
    it("should publish InviteList on init (but not Invite)", async () => {
      const manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should publish InviteList
      const inviteListEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      )
      expect(inviteListEvents.length).toBeGreaterThan(0)

      // Should NOT publish Invite (DeviceManager has no device identity)
      const inviteEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1]?.startsWith("double-ratchet/invites/"))
      )
      expect(inviteEvents.length).toBe(0)
    })

    it("should start with empty device list", async () => {
      const manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const inviteList = manager.getInviteList()
      expect(inviteList).not.toBeNull()

      // DeviceManager no longer auto-adds a device (client must add via DelegateManager flow)
      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(0)
    })
  })

  describe("addDevice()", () => {
    it("should add device to InviteList and publish", async () => {
      const manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const initialPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      ).length

      // Simplified payload format - only identityPubkey
      const payload: DelegatePayload = {
        identityPubkey: getPublicKey(generateSecretKey()),
      }

      await manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(1)
      const device = devices[0]
      expect(device.identityPubkey).toBe(payload.identityPubkey)

      const finalPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      ).length
      expect(finalPublishCount).toBeGreaterThan(initialPublishCount)
    })

    it("should use identityPubkey as device identifier", async () => {
      const manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const delegateIdentityPubkey = getPublicKey(generateSecretKey())
      const payload: DelegatePayload = {
        identityPubkey: delegateIdentityPubkey,
      }

      await manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(1)
      expect(devices[0].identityPubkey).toBe(delegateIdentityPubkey)

      // Can retrieve by identityPubkey
      const device = manager.getInviteList()?.getDevice(delegateIdentityPubkey)
      expect(device).toBeDefined()
      expect(device?.identityPubkey).toBe(delegateIdentityPubkey)
    })
  })

  describe("revokeDevice()", () => {
    it("should remove device from InviteList by identityPubkey", async () => {
      const manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const identityPubkey = getPublicKey(generateSecretKey())
      const payload: DelegatePayload = {
        identityPubkey,
      }
      await manager.addDevice(payload)

      expect(manager.getOwnDevices().length).toBe(1)

      await manager.revokeDevice(identityPubkey)

      expect(manager.getOwnDevices().length).toBe(0)
    })
  })

  describe("getters", () => {
    let manager: DeviceManager

    beforeEach(async () => {
      manager = new DeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()
    })

    it("getOwnerPublicKey() should return owner pubkey", () => {
      expect(manager.getOwnerPublicKey()).toBe(ownerPublicKey)
    })

    it("getInviteList() should return InviteList", () => {
      const list = manager.getInviteList()
      expect(list).not.toBeNull()
      expect(list?.ownerPublicKey).toBe(ownerPublicKey)
    })
  })
})

describe("DeviceManager Integration", () => {
  let publishedEvents: any[]
  let subscribers: Array<{ filter: any; callback: (event: any) => void }>
  let signingKeys: Map<string, Uint8Array>

  const matchesFilter = (event: any, filter: any): boolean => {
    if (filter.kinds && !filter.kinds.includes(event.kind)) return false
    if (filter.authors && !filter.authors.includes(event.pubkey)) return false
    if (filter["#d"]) {
      const dTag = event.tags.find((t: string[]) => t[0] === "d")?.[1]
      if (!filter["#d"].includes(dTag)) return false
    }
    if (filter["#l"]) {
      const lTags = event.tags
        .filter((t: string[]) => t[0] === "l")
        .map((t: string[]) => t[1])
      if (!filter["#l"].some((l: string) => lTags.includes(l))) return false
    }
    if (filter["#p"]) {
      const pTags = event.tags
        .filter((t: string[]) => t[0] === "p")
        .map((t: string[]) => t[1])
      if (!filter["#p"].some((p: string) => pTags.includes(p))) return false
    }
    return true
  }

  const createNostrSubscribe = (): NostrSubscribe => {
    return vi.fn((filter, onEvent) => {
      const sub = { filter, callback: onEvent }
      subscribers.push(sub)

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
      let signedEvent = event
      if (!event.sig && event.pubkey && signingKeys.has(event.pubkey)) {
        const privkey = signingKeys.get(event.pubkey)!
        signedEvent = finalizeEvent(event, privkey)
      }

      publishedEvents.push(signedEvent)

      for (const sub of subscribers) {
        if (matchesFilter(signedEvent, sub.filter)) {
          setTimeout(() => sub.callback(signedEvent), 5)
        }
      }

      return signedEvent
    }) as unknown as NostrPublish
  }

  const registerSigningKey = (pubkey: string, privkey: Uint8Array) => {
    signingKeys.set(pubkey, privkey)
  }

  beforeEach(() => {
    publishedEvents = []
    subscribers = []
    signingKeys = new Map()
  })

  it("DeviceManager adds delegate, delegate activates via waitForActivation", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // 1. Create DelegateManager (device identity)
    const { manager: delegateManager, payload } = DelegateManager.create({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    // Register delegate's signing key
    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    await delegateManager.init()
    const activationPromise = delegateManager.waitForActivation(5000)

    // 2. Create DeviceManager (authority)
    const deviceManager = new DeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await deviceManager.init()

    // 3. Add the delegate device
    await deviceManager.addDevice(payload)

    const devices = deviceManager.getOwnDevices()
    expect(devices.length).toBe(1)
    expect(devices[0].identityPubkey).toBe(payload.identityPubkey)

    await new Promise((resolve) => setTimeout(resolve, 50))

    // 4. Delegate should activate
    const activatedOwnerPubkey = await activationPromise

    expect(activatedOwnerPubkey).toBe(ownerPublicKey)
    expect(delegateManager.getOwnerPublicKey()).toBe(ownerPublicKey)
  })

  it("main device follows same pairing flow as delegate device", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // 1. Create DeviceManager (authority - uses main key)
    const deviceManager = new DeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })
    await deviceManager.init()

    // 2. Create DelegateManager for main device identity (same flow as delegate!)
    const { manager: mainDelegateManager, payload: mainPayload } = DelegateManager.create({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    registerSigningKey(mainDelegateManager.getIdentityPublicKey(), mainDelegateManager.getIdentityKey())
    await mainDelegateManager.init()

    // 3. Add main device to InviteList (same as adding any device)
    await deviceManager.addDevice(mainPayload)

    const devices = deviceManager.getOwnDevices()
    expect(devices.length).toBe(1)
    expect(devices[0].identityPubkey).toBe(mainPayload.identityPubkey)

    // 4. Wait for activation (same as any device!)
    const ownerPubkey = await mainDelegateManager.waitForActivation(5000)
    expect(ownerPubkey).toBe(ownerPublicKey)

    // Main device now has separate identity key (not main key!)
    expect(mainDelegateManager.getIdentityPublicKey()).not.toBe(ownerPublicKey)
  })

  it("DeviceManager revokes delegate, delegate detects revocation", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const { manager: delegateManager, payload } = DelegateManager.create({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    await delegateManager.init()
    const activationPromise = delegateManager.waitForActivation(5000)

    const deviceManager = new DeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await deviceManager.init()
    await deviceManager.addDevice(payload)

    await new Promise((resolve) => setTimeout(resolve, 50))
    await activationPromise

    const initialRevoked = await delegateManager.isRevoked()
    expect(initialRevoked).toBe(false)

    // Revoke by identityPubkey
    await deviceManager.revokeDevice(payload.identityPubkey)

    await new Promise((resolve) => setTimeout(resolve, 50))

    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })

  it("delegate cannot activate if not added to InviteList", async () => {
    const { manager: delegateManager } = DelegateManager.create({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    await delegateManager.init()

    await expect(delegateManager.waitForActivation(200)).rejects.toThrow(
      "Activation timeout"
    )
  })

  it("external user can discover delegate via owner's InviteList", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const { payload } = DelegateManager.create({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    const deviceManager = new DeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    await deviceManager.init()
    await deviceManager.addDevice(payload)

    await new Promise((resolve) => setTimeout(resolve, 50))

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
    expect(devices.length).toBe(1)
    expect(devices[0].identityPubkey).toBe(payload.identityPubkey)
  })
})
