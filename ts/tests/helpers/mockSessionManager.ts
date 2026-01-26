import { vi } from "vitest"
import { DeviceManager, DelegateManager } from "../../src/DeviceManager"
import {
  Filter,
  finalizeEvent,
  generateSecretKey,
  getPublicKey,
  UnsignedEvent,
  VerifiedEvent,
} from "nostr-tools"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { MockRelay } from "./mockRelay"

// Store delegate storage and keys for reuse across restarts
const delegateStorages = new Map<string, InMemoryStorageAdapter>()
const delegateKeys = new Map<string, Uint8Array>()

export const createMockSessionManager = async (
  deviceId: string,
  sharedMockRelay?: MockRelay,
  existingSecretKey?: Uint8Array,
  existingStorage?: InMemoryStorageAdapter
) => {
  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockStorage = existingStorage || new InMemoryStorageAdapter()
  // Use existing delegate storage if available (for restarts)
  const storageKey = `${publicKey}:${deviceId}`
  const delegateStorage = delegateStorages.get(storageKey) || new InMemoryStorageAdapter()
  delegateStorages.set(storageKey, delegateStorage)
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  const mockRelay = sharedMockRelay || new MockRelay()

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return mockRelay.subscribe(filter, onEvent)
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
    return await mockRelay.publish(event, secretKey)
  })

  // Create DeviceManager for InviteList authority
  const deviceManager = new DeviceManager({
    ownerPublicKey: publicKey,
    identityKey: secretKey,
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })

  await deviceManager.init()

  // Create DelegateManager for device identity (same flow as any device!)
  // Need separate publish function that signs with delegate key
  let delegatePrivateKey: Uint8Array | null = null

  const delegateSubscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return mockRelay.subscribe(filter, onEvent)
    })

  const delegatePublish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      const verifiedEvent = event as VerifiedEvent
      // Manually add to relay's events array since we bypass the normal publish flow
      ;(mockRelay as any).events.push(verifiedEvent)
      for (const sub of (mockRelay as any).subscribers.values()) {
        ;(mockRelay as any).deliverToSubscriber(sub, verifiedEvent)
      }
      return verifiedEvent
    }
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    // Add signed event to relay
    ;(mockRelay as any).events.push(signedEvent)
    for (const sub of (mockRelay as any).subscribers.values()) {
      ;(mockRelay as any).deliverToSubscriber(sub, signedEvent)
    }
    return signedEvent
  })

  let delegateManager: DelegateManager
  const existingDelegateKey = delegateKeys.get(storageKey)

  if (existingDelegateKey) {
    // Restore existing delegate (for restarts)
    delegateManager = DelegateManager.restore({
      devicePublicKey: getPublicKey(existingDelegateKey),
      devicePrivateKey: existingDelegateKey,
      nostrSubscribe: delegateSubscribe,
      nostrPublish: delegatePublish,
      storage: delegateStorage,
    })
    delegatePrivateKey = existingDelegateKey
    await delegateManager.init()

    // Device is already activated, just need to activate with stored owner
    const storedOwner = await delegateStorage.get<string>('v1/device-manager/owner-pubkey')
    if (storedOwner) {
      await delegateManager.activate(storedOwner)
    } else {
      // Fall back to waiting for activation
      await delegateManager.waitForActivation(5000)
    }
  } else {
    // Create new delegate
    const createResult = DelegateManager.create({
      nostrSubscribe: delegateSubscribe,
      nostrPublish: delegatePublish,
      storage: delegateStorage,
    })
    delegateManager = createResult.manager
    const payload = createResult.payload

    delegatePrivateKey = delegateManager.getIdentityKey()
    delegateKeys.set(storageKey, delegatePrivateKey) // Save for future restarts
    await delegateManager.init()

    // Add device to InviteList
    await deviceManager.addDevice(payload)

    // Wait for activation
    await delegateManager.waitForActivation(5000)
  }

  // Create SessionManager using DelegateManager
  const manager = delegateManager.createSessionManager()
  await manager.init()

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    deviceManager,
    delegateManager,
    subscribe,
    publish,
    onEvent,
    mockStorage,
    storageSpy,
    secretKey,
    publicKey,
    relay: mockRelay,
  }
}

export const createMockDelegateSessionManager = async (
  deviceId: string,
  sharedMockRelay: MockRelay,
  mainDeviceManager: DeviceManager
) => {
  const mockStorage = new InMemoryStorageAdapter()
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  // Context to hold the delegate's private key for signing
  // Will be set after DelegateManager is created
  let delegatePrivateKey: Uint8Array | null = null

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return sharedMockRelay.subscribe(filter, onEvent)
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    // Already signed, just add to relay
    if ('sig' in event && event.sig) {
      const verifiedEvent = event as VerifiedEvent
      // Manually add to relay's events array since we bypass the normal publish flow
      ;(sharedMockRelay as any).events.push(verifiedEvent)
      for (const sub of (sharedMockRelay as any).subscribers.values()) {
        ;(sharedMockRelay as any).deliverToSubscriber(sub, verifiedEvent)
      }
      return verifiedEvent
    }
    // Unsigned event - sign with delegate's private key (for Invite events from DeviceManager)
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    // Add signed event to relay
    ;(sharedMockRelay as any).events.push(signedEvent)
    for (const sub of (sharedMockRelay as any).subscribers.values()) {
      ;(sharedMockRelay as any).deliverToSubscriber(sub, signedEvent)
    }
    return signedEvent
  })

  // Create delegate DelegateManager
  const { manager: delegateManager, payload } = DelegateManager.create({
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })

  // Get the delegate's private key for signing
  delegatePrivateKey = delegateManager.getIdentityKey()

  await delegateManager.init()

  // Main device adds delegate to its InviteList
  await mainDeviceManager.addDevice(payload)

  // Delegate waits for activation
  await delegateManager.waitForActivation(5000)

  // Use DelegateManager to create properly configured SessionManager
  const manager = delegateManager.createSessionManager()
  await manager.init()

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    delegateManager,
    subscribe,
    publish,
    onEvent,
    mockStorage,
    storageSpy,
    publicKey: delegateManager.getIdentityPublicKey(),
    relay: sharedMockRelay,
  }
}
