import { describe, it, expect, vi, beforeEach } from "vitest"
import { ApplicationManager, DelegateManager, DelegatePayload } from "../src/ApplicationManager"
import { NostrSubscribe, NostrPublish, APPLICATION_KEYS_EVENT_KIND, INVITE_EVENT_KIND } from "../src/types"
import { generateSecretKey, getPublicKey, finalizeEvent } from "nostr-tools"
import { InMemoryStorageAdapter } from "../src/StorageAdapter"
import { ApplicationKeys } from "../src/ApplicationKeys"

describe("DelegateManager", () => {
  let nostrSubscribe: NostrSubscribe
  let nostrPublish: NostrPublish
  let publishedEvents: any[]
  let subscriptions: Map<string, { filter: any; callback: (event: any) => void }>

  beforeEach(() => {
    publishedEvents = []
    subscriptions = new Map()

    nostrSubscribe = vi.fn((filter, onEvent) => {
      const key = JSON.stringify(filter)
      subscriptions.set(key, { filter, callback: onEvent })
      return () => {
        subscriptions.delete(key)
      }
    }) as unknown as NostrSubscribe

    nostrPublish = vi.fn(async (event) => {
      publishedEvents.push(event)
      return event
    }) as unknown as NostrPublish
  })

  describe("constructor and init()", () => {
    it("should create a DelegateManager", async () => {
      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(DelegateManager)
    })

    it("should generate identity keypair on init", async () => {
      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const payload = manager.getRegistrationPayload()

      expect(payload.identityPubkey).toBeDefined()
      expect(payload.identityPubkey).toHaveLength(64)
      expect(manager.getIdentityPublicKey()).toBe(payload.identityPubkey)

      const privkey = manager.getIdentityKey()
      expect(privkey).toBeInstanceOf(Uint8Array)
      expect((privkey as Uint8Array).length).toBe(32)
    })

    it("should return simplified payload with only identityPubkey", async () => {
      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const payload = manager.getRegistrationPayload()

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
    it("should publish Invite event (not ApplicationKeys)", async () => {
      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      // Should NOT publish ApplicationKeys (only ApplicationManager does that)
      const applicationKeysEvents = publishedEvents.filter(
        (e) => e.kind === APPLICATION_KEYS_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/application-keys")
      )
      expect(applicationKeysEvents.length).toBe(0)

      // Should publish its own Invite event
      const inviteEvents = publishedEvents.filter(
        (e) => e.kind === INVITE_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1]?.startsWith("double-ratchet/invites/"))
      )
      expect(inviteEvents.length).toBe(1)
    })

    it("should create and store Invite on init", async () => {
      const storage = new InMemoryStorageAdapter()

      const manager = new DelegateManager({
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

      // DelegateManager uses v1 for storage version
      await storage.put("v1/device-manager/owner-pubkey", ownerPubkey)

      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
        storage,
      })

      await manager.init()

      expect(manager.getOwnerPublicKey()).toBe(ownerPubkey)
    })

    it("should restore identity keys from storage on restart", async () => {
      const storage = new InMemoryStorageAdapter()

      // First instance - generates keys
      const manager1 = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager1.init()
      const originalPubkey = manager1.getIdentityPublicKey()
      const originalPrivkey = manager1.getIdentityKey()

      // Second instance with same storage - should restore keys
      const manager2 = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
        storage,
      })
      await manager2.init()

      expect(manager2.getIdentityPublicKey()).toBe(originalPubkey)
      expect(Array.from(manager2.getIdentityKey())).toEqual(Array.from(originalPrivkey))
    })
  })

  describe("waitForActivation()", () => {
    it("should subscribe to ApplicationKeys events", async () => {
      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()

      const activationPromise = manager.waitForActivation(100)

      expect(nostrSubscribe).toHaveBeenCalled()
      const calls = (nostrSubscribe as any).mock.calls
      const applicationKeysCall = calls.find(
        (call: any) => call[0].kinds?.includes(APPLICATION_KEYS_EVENT_KIND)
      )
      expect(applicationKeysCall).toBeDefined()

      await expect(activationPromise).rejects.toThrow("Activation timeout")
    })

    it("should resolve when own identityPubkey appears in an ApplicationKeys", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const payload = manager.getRegistrationPayload()

      const activationPromise = manager.waitForActivation(5000)

      await new Promise((resolve) => setTimeout(resolve, 50))

      // Simplified tag format: ["device", identityPubkey, createdAt]
      const applicationKeysEvent = finalizeEvent(
        {
          kind: APPLICATION_KEYS_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/application-keys"],
            ["version", "1"],
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
        key.includes(String(APPLICATION_KEYS_EVENT_KIND))
      )
      if (subscriptionKey) {
        const sub = subscriptions.get(subscriptionKey)
        sub?.callback(applicationKeysEvent)
      }

      const result = await activationPromise
      expect(result).toBe(ownerPublicKey)
    })

    it("should resolve immediately if already activated", async () => {
      const storage = new InMemoryStorageAdapter()
      const ownerPubkey = getPublicKey(generateSecretKey())

      // DelegateManager uses v1 for storage version
      await storage.put("v1/device-manager/owner-pubkey", ownerPubkey)

      const manager = new DelegateManager({
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
    it("should return false when device is in ApplicationKeys", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const payload = manager.getRegistrationPayload()

      const activationPromise = manager.waitForActivation(5000)
      await new Promise((resolve) => setTimeout(resolve, 50))

      const applicationKeysEvent = finalizeEvent(
        {
          kind: APPLICATION_KEYS_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/application-keys"],
            ["version", "1"],
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
        key.includes(String(APPLICATION_KEYS_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.callback(applicationKeysEvent)
      }

      await activationPromise

      const isRevokedSubscribe = vi.fn((filter, onEvent) => {
        if (
          filter.kinds?.includes(APPLICATION_KEYS_EVENT_KIND) &&
          filter.authors?.includes(ownerPublicKey)
        ) {
          setTimeout(() => onEvent(applicationKeysEvent), 10)
        }
        return () => {}
      }) as unknown as NostrSubscribe

      ;(manager as any).nostrSubscribe = isRevokedSubscribe

      const revoked = await manager.isRevoked()
      expect(revoked).toBe(false)
    })

    it("should return true when device is removed from ApplicationKeys", async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const manager = new DelegateManager({
        nostrSubscribe,
        nostrPublish,
      })

      await manager.init()
      const payload = manager.getRegistrationPayload()

      const activationPromise = manager.waitForActivation(5000)
      await new Promise((resolve) => setTimeout(resolve, 50))

      const applicationKeysEvent = finalizeEvent(
        {
          kind: APPLICATION_KEYS_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [
            ["d", "double-ratchet/application-keys"],
            ["version", "1"],
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
        key.includes(String(APPLICATION_KEYS_EVENT_KIND))
      )
      if (subscriptionKey) {
        subscriptions.get(subscriptionKey)?.callback(applicationKeysEvent)
      }

      await activationPromise

      // Device is revoked by simply not being in the list anymore
      const revokedApplicationKeysEvent = finalizeEvent(
        {
          kind: APPLICATION_KEYS_EVENT_KIND,
          created_at: Math.floor(Date.now() / 1000) + 1,
          tags: [
            ["d", "double-ratchet/application-keys"],
            ["version", "1"],
            // No device tags - the device is simply not present
          ],
          content: "",
        },
        ownerPrivateKey
      )

      const isRevokedSubscribe = vi.fn((filter, onEvent) => {
        if (
          filter.kinds?.includes(APPLICATION_KEYS_EVENT_KIND) &&
          filter.authors?.includes(ownerPublicKey)
        ) {
          setTimeout(() => onEvent(revokedApplicationKeysEvent), 10)
        }
        return () => {}
      }) as unknown as NostrSubscribe

      ;(manager as any).nostrSubscribe = isRevokedSubscribe

      const revoked = await manager.isRevoked()
      expect(revoked).toBe(true)
    })
  })
})

describe("ApplicationManager - Authority", () => {
  let nostrPublish: NostrPublish
  let publishedEvents: any[]

  beforeEach(() => {
    publishedEvents = []

    nostrPublish = vi.fn(async (event) => {
      publishedEvents.push(event)
      return event
    }) as unknown as NostrPublish
  })

  describe("constructor", () => {
    it("should create a ApplicationManager", () => {
      const manager = new ApplicationManager({
        nostrPublish,
      })

      expect(manager).toBeInstanceOf(ApplicationManager)
    })
  })

  describe("init()", () => {
    it("should not auto-publish ApplicationKeys on init", async () => {
      const manager = new ApplicationManager({
        nostrPublish,
      })

      await manager.init()

      // Init does NOT auto-publish - client must call publish() explicitly
      expect(publishedEvents.length).toBe(0)

      // Calling publish() will publish
      await manager.publish()
      const applicationKeysEvents = publishedEvents.filter(
        (e) => e.kind === APPLICATION_KEYS_EVENT_KIND && e.tags?.some((t: string[]) => t[0] === "d" && t[1] === "double-ratchet/application-keys")
      )
      expect(applicationKeysEvents.length).toBe(1)
    })

    it("should start with empty device list", async () => {
      const manager = new ApplicationManager({
        nostrPublish,
      })

      await manager.init()

      const applicationKeys = manager.getApplicationKeys()
      expect(applicationKeys).not.toBeNull()

      // ApplicationManager no longer auto-adds a device (client must add via DelegateManager flow)
      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(0)
    })
  })

  describe("addDevice()", () => {
    it("should add device to ApplicationKeys (local only - publish separately)", async () => {
      const manager = new ApplicationManager({
        nostrPublish,
      })
      await manager.init()

      // Simplified payload format - only identityPubkey
      const payload: DelegatePayload = {
        identityPubkey: getPublicKey(generateSecretKey()),
      }

      manager.addDevice(payload) // Synchronous - local only

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(1)
      const device = devices[0]
      expect(device.identityPubkey).toBe(payload.identityPubkey)

      // Not published yet
      expect(publishedEvents.length).toBe(0)

      // Must call publish() to send to relay
      await manager.publish()
      expect(publishedEvents.length).toBe(1)
    })

    it("should use identityPubkey as device identifier", async () => {
      const manager = new ApplicationManager({
        nostrPublish,
      })
      await manager.init()

      const delegateIdentityPubkey = getPublicKey(generateSecretKey())
      const payload: DelegatePayload = {
        identityPubkey: delegateIdentityPubkey,
      }

      manager.addDevice(payload)

      const devices = manager.getOwnDevices()
      expect(devices.length).toBe(1)
      expect(devices[0].identityPubkey).toBe(delegateIdentityPubkey)

      // Can retrieve by identityPubkey
      const device = manager.getApplicationKeys()?.getDevice(delegateIdentityPubkey)
      expect(device).toBeDefined()
      expect(device?.identityPubkey).toBe(delegateIdentityPubkey)
    })
  })

  describe("revokeDevice()", () => {
    it("should remove device from ApplicationKeys by identityPubkey", async () => {
      const manager = new ApplicationManager({
        nostrPublish,
      })
      await manager.init()

      const identityPubkey = getPublicKey(generateSecretKey())
      const payload: DelegatePayload = {
        identityPubkey,
      }
      manager.addDevice(payload)

      expect(manager.getOwnDevices().length).toBe(1)

      manager.revokeDevice(identityPubkey)

      expect(manager.getOwnDevices().length).toBe(0)
    })
  })

  describe("getters", () => {
    let manager: ApplicationManager

    beforeEach(async () => {
      manager = new ApplicationManager({
        nostrPublish,
      })
      await manager.init()
    })

    it("getApplicationKeys() should return ApplicationKeys", () => {
      const list = manager.getApplicationKeys()
      expect(list).not.toBeNull()
    })
  })
})

describe("ApplicationManager Integration", () => {
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

  it("ApplicationManager adds delegate, delegate activates via waitForActivation", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    // 1. Create DelegateManager (device identity)
    const delegateManager = new DelegateManager({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await delegateManager.init()
    const payload = delegateManager.getRegistrationPayload()

    // Register delegate's signing key
    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    const activationPromise = delegateManager.waitForActivation(5000)

    // 2. Create ApplicationManager (authority) - only needs nostrPublish
    // The signing is done by the publish implementation that uses signingKeys
    const deviceManagerPublish = createNostrPublish()
    const originalPublish = deviceManagerPublish
    const signedPublish = vi.fn(async (event) => {
      // Add owner's signature if not already signed
      const signedEvent = !event.sig && ownerPrivateKey
        ? finalizeEvent(event, ownerPrivateKey)
        : event
      return originalPublish(signedEvent)
    }) as unknown as NostrPublish

    const deviceManager = new ApplicationManager({
      nostrPublish: signedPublish,
      storage: new InMemoryStorageAdapter(),
    })

    await deviceManager.init()

    // 3. Add the delegate device (local only) then publish
    deviceManager.addDevice(payload)
    await deviceManager.publish()

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

    // 1. Create ApplicationManager (authority) with signed publish
    const deviceManagerPublish = createNostrPublish()
    const originalPublish = deviceManagerPublish
    const signedPublish = vi.fn(async (event) => {
      const signedEvent = !event.sig && ownerPrivateKey
        ? finalizeEvent(event, ownerPrivateKey)
        : event
      return originalPublish(signedEvent)
    }) as unknown as NostrPublish

    const deviceManager = new ApplicationManager({
      nostrPublish: signedPublish,
      storage: new InMemoryStorageAdapter(),
    })
    await deviceManager.init()

    // 2. Create DelegateManager for main device identity (same flow as delegate!)
    const mainDelegateManager = new DelegateManager({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await mainDelegateManager.init()
    const mainPayload = mainDelegateManager.getRegistrationPayload()

    registerSigningKey(mainDelegateManager.getIdentityPublicKey(), mainDelegateManager.getIdentityKey())

    // 3. Add main device to ApplicationKeys (same as adding any device) then publish
    deviceManager.addDevice(mainPayload)
    await deviceManager.publish()

    const devices = deviceManager.getOwnDevices()
    expect(devices.length).toBe(1)
    expect(devices[0].identityPubkey).toBe(mainPayload.identityPubkey)

    // 4. Wait for activation (same as any device!)
    const ownerPubkey = await mainDelegateManager.waitForActivation(5000)
    expect(ownerPubkey).toBe(ownerPublicKey)

    // Main device now has separate identity key (not main key!)
    expect(mainDelegateManager.getIdentityPublicKey()).not.toBe(ownerPublicKey)
  })

  it("ApplicationManager revokes delegate, delegate detects revocation", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const delegateManager = new DelegateManager({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await delegateManager.init()
    const payload = delegateManager.getRegistrationPayload()

    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    const activationPromise = delegateManager.waitForActivation(5000)

    // Create ApplicationManager with signed publish
    const deviceManagerPublish = createNostrPublish()
    const originalPublish = deviceManagerPublish
    const signedPublish = vi.fn(async (event) => {
      const signedEvent = !event.sig && ownerPrivateKey
        ? finalizeEvent(event, ownerPrivateKey)
        : event
      return originalPublish(signedEvent)
    }) as unknown as NostrPublish

    const deviceManager = new ApplicationManager({
      nostrPublish: signedPublish,
      storage: new InMemoryStorageAdapter(),
    })

    await deviceManager.init()
    deviceManager.addDevice(payload)
    await deviceManager.publish()

    await new Promise((resolve) => setTimeout(resolve, 50))
    await activationPromise

    const initialRevoked = await delegateManager.isRevoked()
    expect(initialRevoked).toBe(false)

    // Revoke by identityPubkey and publish
    deviceManager.revokeDevice(payload.identityPubkey)
    await deviceManager.publish()

    await new Promise((resolve) => setTimeout(resolve, 50))

    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })

  it("delegate cannot activate if not added to ApplicationKeys", async () => {
    const delegateManager = new DelegateManager({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
      storage: new InMemoryStorageAdapter(),
    })

    await delegateManager.init()

    registerSigningKey(delegateManager.getIdentityPublicKey(), delegateManager.getIdentityKey())

    await expect(delegateManager.waitForActivation(200)).rejects.toThrow(
      "Activation timeout"
    )
  })

  it("external user can discover delegate via owner's ApplicationKeys", async () => {
    const ownerPrivateKey = generateSecretKey()
    const ownerPublicKey = getPublicKey(ownerPrivateKey)

    registerSigningKey(ownerPublicKey, ownerPrivateKey)

    const delegateManager = new DelegateManager({
      nostrSubscribe: createNostrSubscribe(),
      nostrPublish: createNostrPublish(),
    })
    await delegateManager.init()
    const payload = delegateManager.getRegistrationPayload()

    // Create ApplicationManager with signed publish
    const deviceManagerPublish = createNostrPublish()
    const originalPublish = deviceManagerPublish
    const signedPublish = vi.fn(async (event) => {
      const signedEvent = !event.sig && ownerPrivateKey
        ? finalizeEvent(event, ownerPrivateKey)
        : event
      return originalPublish(signedEvent)
    }) as unknown as NostrPublish

    const deviceManager = new ApplicationManager({
      nostrPublish: signedPublish,
    })

    await deviceManager.init()
    deviceManager.addDevice(payload)
    await deviceManager.publish()

    await new Promise((resolve) => setTimeout(resolve, 50))

    const externalSubscribe = createNostrSubscribe()

    const fetchedList = await new Promise<ApplicationKeys | null>((resolve) => {
      let latestEvent: any = null
      const unsub = externalSubscribe(
        {
          kinds: [APPLICATION_KEYS_EVENT_KIND],
          authors: [ownerPublicKey],
          "#d": ["double-ratchet/application-keys"],
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
            resolve(ApplicationKeys.fromEvent(latestEvent))
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
