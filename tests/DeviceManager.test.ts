import { describe, it, expect, vi, beforeEach } from "vitest"
import { OwnerDeviceManager, DelegateDeviceManager, DelegateDevicePayload } from "../src/DeviceManager"
import { NostrSubscribe, NostrPublish, INVITE_LIST_EVENT_KIND, INVITE_EVENT_KIND } from "../src/types"
import { generateSecretKey, getPublicKey, finalizeEvent } from "nostr-tools"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { InviteList } from "../src/InviteList"

describe("DeviceManager - Delegate Device", () => {
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

  describe("createDelegate()", () => {
    it("should create a DelegateDeviceManager", () => {
      const { manager } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(DelegateDeviceManager)
    })

    it("should generate identity keypair", () => {
      const { manager, payload } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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

    it("should return payload with identity fields only", () => {
      const { payload } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      // New payload only contains identity info
      expect(payload.deviceId).toBe("delegate-device")
      expect(payload.deviceLabel).toBe("My Phone")
      expect(payload.identityPubkey).toBeDefined()
      // No longer includes ephemeral keys or shared secret in payload
      expect((payload as any).ephemeralPubkey).toBeUndefined()
      expect((payload as any).sharedSecret).toBeUndefined()
    })
  })

  describe("init()", () => {
    it("should publish Invite event (not InviteList)", async () => {
      const { manager } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should NOT publish InviteList (only owner does that)
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

      const { manager } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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

      await storage.put("v2/device-manager/owner-pubkey", ownerPubkey)

      const { manager } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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
      const { manager } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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

    it("should resolve when own deviceId appears in an InviteList with matching identityPubkey", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const { manager, payload } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const activationPromise = manager.waitForActivation(5000)

      await new Promise((resolve) => setTimeout(resolve, 50))

      // New tag format: ["device", deviceId, identityPubkey, createdAt]
      const inviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["version", "2"],
            [
              "device",
              payload.deviceId,
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

      await storage.put("v2/device-manager/owner-pubkey", ownerPubkey)

      const { manager } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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

      const { manager, payload } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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
            ["version", "2"],
            [
              "device",
              payload.deviceId,
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

      const { manager, payload } = DelegateDeviceManager.create({
        deviceId: "delegate-device",
        deviceLabel: "My Phone",
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
            ["version", "2"],
            [
              "device",
              payload.deviceId,
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

      const revokedInviteListEvent = finalizeEvent(
        {
          kind: INVITE_LIST_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000) + 1,
          tags: [
            ["d", "double-ratchet/invite-list"],
            ["version", "2"],
            ["removed", payload.deviceId],
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

describe("DeviceManager - Main Device", () => {
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
    it("should create an OwnerDeviceManager", () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(OwnerDeviceManager)
    })
  })

  describe("init()", () => {
    it("should create InviteList with own device", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const inviteList = manager.getInviteList()
      expect(inviteList).not.toBeNull()

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(1)
      expect(devices[0].deviceId).toBe("main-device")
      expect(devices[0].deviceLabel).toBe("Main Device")
    })

    it("should publish both InviteList and Invite on init", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should publish InviteList
      const inviteListEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      )
      expect(inviteListEvents.length).toBeGreaterThan(0)

      // Should also publish device's Invite
      const inviteEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1]?.startsWith("double-ratchet/invites/"))
      )
      expect(inviteEvents.length).toBe(1)
    })

    it("should create and store Invite on init", async () => {
      const storage = new InMemoryStorageAdapter()

      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
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

    it("should reload same Invite from storage", async () => {
      const storage = new InMemoryStorageAdapter()

      const manager1 = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager1.init()

      const ephemeralKey1 = manager1.getInvite()?.inviterEphemeralPublicKey

      const manager2 = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager2.init()

      const ephemeralKey2 = manager2.getInvite()?.inviterEphemeralPublicKey
      expect(ephemeralKey2).toBe(ephemeralKey1)
    })
  })

  describe("addDevice()", () => {
    it("should add device to InviteList and publish", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const initialPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      ).length

      // New payload format - identity only
      const payload: DelegateDevicePayload = {
        deviceId: "secondary-device",
        deviceLabel: "Secondary Device",
        identityPubkey: getPublicKey(generateSecretKey()),
      }

      await manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(2)
      const secondaryDevice = devices.find((d) => d.deviceId === "secondary-device")
      expect(secondaryDevice).toBeDefined()
      expect(secondaryDevice?.deviceLabel).toBe("Secondary Device")

      const finalPublishCount = publishedEvents.filter(
        (e) => e.kind === INVITE_LIST_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/invite-list")
      ).length
      expect(finalPublishCount).toBeGreaterThan(initialPublishCount)
    })

    it("should include identityPubkey for delegate devices", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const delegateIdentityPubkey = getPublicKey(generateSecretKey())
      const payload: DelegateDevicePayload = {
        deviceId: "delegate-device",
        deviceLabel: "Delegate Device",
        identityPubkey: delegateIdentityPubkey,
      }

      await manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      const delegateDevice = devices.find((d) => d.deviceId === "delegate-device")
      expect(delegateDevice?.identityPubkey).toBe(delegateIdentityPubkey)
    })
  })

  describe("revokeDevice()", () => {
    it("should remove device from InviteList", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const payload: DelegateDevicePayload = {
        deviceId: "secondary-device",
        deviceLabel: "Secondary Device",
        identityPubkey: getPublicKey(generateSecretKey()),
      }
      await manager.addDevice(payload)

      expect(manager.getOwnDevices().length).toBe(2)

      await manager.revokeDevice("secondary-device")

      expect(manager.getOwnDevices().length).toBe(1)
      expect(manager.getOwnDevices()[0].deviceId).toBe("main-device")
    })

    it("should not allow revoking own device", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      await expect(manager.revokeDevice("main-device")).rejects.toThrow()
    })
  })

  describe("getters", () => {
    let manager: OwnerDeviceManager

    beforeEach(async () => {
      manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()
    })

    it("getIdentityPublicKey() should return owner pubkey", () => {
      expect(manager.getIdentityPublicKey()).toBe(ownerPublicKey)
    })

    it("getIdentityKey() should return owner privkey", () => {
      expect(manager.getIdentityKey()).toEqual(ownerPrivateKey)
    })

    it("getDeviceId() should return device ID", () => {
      expect(manager.getDeviceId()).toBe("main-device")
    })

    it("getInvite() should return Invite with ephemeral keys", () => {
      const invite = manager.getInvite()
      expect(invite).not.toBeNull()
      expect(invite?.inviterEphemeralPublicKey).toHaveLength(64)
      expect(invite?.inviterEphemeralPrivateKey).toBeInstanceOf(Uint8Array)
      expect(invite?.sharedSecret).toHaveLength(64)
    })
  })

  describe("rotateInvite()", () => {
    it("should create new Invite with different keys", async () => {
      const manager = new OwnerDeviceManager({
        ownerPublicKey,
        identityKey: ownerPrivateKey,
        deviceId: "main-device",
        deviceLabel: "Main Device",
        nostrSubscribe,
        nostrPublish,
      })
      await manager.init()

      const originalInvite = manager.getInvite()
      const originalEphemeralKey = originalInvite?.inviterEphemeralPublicKey

      await manager.rotateInvite()

      const newInvite = manager.getInvite()
      expect(newInvite?.inviterEphemeralPublicKey).not.toBe(originalEphemeralKey)
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

  it("main device adds delegate, delegate activates", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const { manager: delegateManager, payload } = DelegateDeviceManager.create({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    // Register delegate's signing key
    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    await delegateManager.init()
    const activationPromise = delegateManager.waitForActivation(5000)

    const mainManager = new OwnerDeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await mainManager.init()
    await mainManager.addDevice(payload)

    const devices = mainManager.getOwnDevices()
    expect(devices.length).toBe(2)
    expect(devices.some((d) => d.deviceId === "phone-123")).toBe(true)

    await new Promise((resolve) => setTimeout(resolve, 50))

    const activatedOwnerPubkey = await activationPromise

    expect(activatedOwnerPubkey).toBe(ownerPublicKey)
    expect(delegateManager.getOwnerPublicKey()).toBe(ownerPublicKey)
  })

  it("main device revokes delegate, delegate detects revocation", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const { manager: delegateManager, payload } = DelegateDeviceManager.create({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    await delegateManager.init()
    const activationPromise = delegateManager.waitForActivation(5000)

    const mainManager = new OwnerDeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
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

    const initialRevoked = await delegateManager.isRevoked()
    expect(initialRevoked).toBe(false)

    await mainManager.revokeDevice("phone-123")

    await new Promise((resolve) => setTimeout(resolve, 50))

    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })

  it("delegate cannot activate if not added to InviteList", async () => {
    const { manager: delegateManager } = DelegateDeviceManager.create({
      deviceId: "phone-orphan",
      deviceLabel: "Orphan Phone",
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

    const { payload } = DelegateDeviceManager.create({
      deviceId: "phone-123",
      deviceLabel: "My Phone",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    const mainManager = new OwnerDeviceManager({
      ownerPublicKey,
      identityKey: ownerPrivateKey,
      deviceId: "main-device",
      deviceLabel: "Main Device",
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })

    await mainManager.init()
    await mainManager.addDevice(payload)

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
    expect(devices.length).toBe(2)

    const delegateDevice = devices.find((d) => d.deviceId === "phone-123")
    expect(delegateDevice).toBeDefined()
    expect(delegateDevice?.identityPubkey).toBe(payload.identityPubkey)
  })
})
