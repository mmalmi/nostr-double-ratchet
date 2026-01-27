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

// Track DeviceManagers per owner (by publicKey) and relay to share InviteList across devices
// Key format: `${relayId}:${publicKey}` where relayId is a unique ID assigned to each relay
const deviceManagers = new Map<string, DeviceManager>()
const deviceManagerStorages = new Map<string, InMemoryStorageAdapter>()
const relayIds = new WeakMap<MockRelay, string>()
let relayCounter = 0

function getRelayId(relay: MockRelay): string {
  let id = relayIds.get(relay)
  if (!id) {
    id = `relay-${++relayCounter}`
    relayIds.set(relay, id)
  }
  return id
}

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

  const mockRelay = sharedMockRelay || new MockRelay()

  // Use existing delegate storage if available (for restarts)
  const storageKey = `${publicKey}:${deviceId}`
  const delegateStorage = delegateStorages.get(storageKey) || new InMemoryStorageAdapter()
  delegateStorages.set(storageKey, delegateStorage)

  // Get or create DeviceManager for this owner+relay (shared across all devices of same owner on same relay)
  const relayId = getRelayId(mockRelay)
  const deviceManagerKey = `${relayId}:${publicKey}`
  let deviceManager = deviceManagers.get(deviceManagerKey)
  let deviceManagerStorage = deviceManagerStorages.get(deviceManagerKey)

  if (!deviceManager || !deviceManagerStorage) {
    deviceManagerStorage = existingStorage || new InMemoryStorageAdapter()
    deviceManagerStorages.set(deviceManagerKey, deviceManagerStorage)

    // DeviceManager publish signs with owner's secret key
    // Use mockRelay.publish() to properly handle replaceable events
    const deviceManagerPublish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
      return await mockRelay.publish(event, secretKey)
    })

    // Create DeviceManager for InviteList authority (only needs nostrPublish)
    deviceManager = new DeviceManager({
      nostrPublish: deviceManagerPublish,
      storage: deviceManagerStorage,
    })

    await deviceManager.init()
    deviceManagers.set(deviceManagerKey, deviceManager)
  }

  const mockStorage = deviceManagerStorage!
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return mockRelay.subscribe(filter, onEvent)
    })

  // Create DelegateManager for device identity
  let delegatePrivateKey: Uint8Array | null = null

  const delegateSubscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return mockRelay.subscribe(filter, onEvent)
    })

  const delegatePublish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      // Already signed - use mockRelay.publish() which will handle it
      return await mockRelay.publish(event as UnsignedEvent)
    }
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    return await mockRelay.publish(event, delegatePrivateKey)
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

    // Add device to InviteList and publish
    deviceManager.addDevice(payload)
    await deviceManager.publish() // Publish InviteList to relay

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
    publish: delegatePublish,
    onEvent,
    mockStorage,
    storageSpy,
    secretKey,
    publicKey,
    relay: mockRelay,
  }
}

export const createMockDelegateSessionManager = async (
  _deviceId: string,
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
  let delegatePrivateKey: Uint8Array | null = null

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return sharedMockRelay.subscribe(filter, onEvent)
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      // Already signed - use mockRelay.publish() which will handle it
      return await sharedMockRelay.publish(event as UnsignedEvent)
    }
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    return await sharedMockRelay.publish(event, delegatePrivateKey)
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

  // Main device adds delegate to its InviteList and publishes
  mainDeviceManager.addDevice(payload)
  await mainDeviceManager.publish()

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

// Reset all tracked state - call this in afterEach/beforeEach
export const resetMockSessionManagerState = () => {
  deviceManagers.clear()
  deviceManagerStorages.clear()
  delegateStorages.clear()
  delegateKeys.clear()
}
