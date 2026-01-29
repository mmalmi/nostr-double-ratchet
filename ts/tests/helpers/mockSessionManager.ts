import { vi } from "vitest"
import { ApplicationManager, DelegateManager } from "../../src/ApplicationManager"
import {
  Filter,
  generateSecretKey,
  getPublicKey,
  UnsignedEvent,
  VerifiedEvent,
} from "nostr-tools"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { MockRelay } from "./mockRelay"

// Track ApplicationManagers per owner (by publicKey) and relay to share ApplicationKeys across devices
// Key format: `${relayId}:${publicKey}` where relayId is a unique ID assigned to each relay
const applicationManagers = new Map<string, ApplicationManager>()
const applicationManagerStorages = new Map<string, InMemoryStorageAdapter>()
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

// Store delegate storage for reuse across restarts
const delegateStorages = new Map<string, InMemoryStorageAdapter>()

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

  // Get or create ApplicationManager for this owner+relay (shared across all devices of same owner on same relay)
  const relayId = getRelayId(mockRelay)
  const applicationManagerKey = `${relayId}:${publicKey}`
  let applicationManager = applicationManagers.get(applicationManagerKey)
  let applicationManagerStorage = applicationManagerStorages.get(applicationManagerKey)

  if (!applicationManager || !applicationManagerStorage) {
    applicationManagerStorage = existingStorage || new InMemoryStorageAdapter()
    applicationManagerStorages.set(applicationManagerKey, applicationManagerStorage)

    // ApplicationManager publish signs with owner's secret key
    // Use mockRelay.publish() to properly handle replaceable events
    const applicationManagerPublish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
      return await mockRelay.publish(event, secretKey)
    })

    // Create ApplicationManager for ApplicationKeys authority (only needs nostrPublish)
    applicationManager = new ApplicationManager({
      nostrPublish: applicationManagerPublish,
      storage: applicationManagerStorage,
    })

    await applicationManager.init()
    applicationManagers.set(applicationManagerKey, applicationManager)
  }

  const mockStorage = applicationManagerStorage!
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

  // Use a holder so the publish function can access the manager's key during init
  const managerHolder: { manager: DelegateManager | null } = { manager: null }

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
    const delegatePrivateKey = managerHolder.manager?.getIdentityKey()
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    return await mockRelay.publish(event, delegatePrivateKey)
  })

  // Create or restore DelegateManager using same storage (auto-restores keys)
  const delegateManager = new DelegateManager({
    nostrSubscribe: delegateSubscribe,
    nostrPublish: delegatePublish,
    storage: delegateStorage,
  })
  managerHolder.manager = delegateManager

  await delegateManager.init()

  // Check if already activated
  const storedOwner = await delegateStorage.get<string>('v1/device-manager/owner-pubkey')
  if (storedOwner) {
    // Already activated, nothing more to do
  } else {
    // New delegate - add to ApplicationKeys and wait for activation
    const payload = delegateManager.getRegistrationPayload()
    applicationManager.addDevice(payload)
    await applicationManager.publish() // Publish ApplicationKeys to relay
    await delegateManager.waitForActivation(5000)
  }

  // Create SessionManager using DelegateManager
  const manager = delegateManager.createSessionManager()
  await manager.init()

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    applicationManager,
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
  mainApplicationManager: ApplicationManager
) => {
  const mockStorage = new InMemoryStorageAdapter()
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  // Use a holder so the publish function can access the manager's key during init
  const managerHolder: { manager: DelegateManager | null } = { manager: null }

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
    const delegatePrivateKey = managerHolder.manager?.getIdentityKey()
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    return await sharedMockRelay.publish(event, delegatePrivateKey)
  })

  // Create delegate DelegateManager
  const delegateManager = new DelegateManager({
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })
  managerHolder.manager = delegateManager

  await delegateManager.init()
  const payload = delegateManager.getRegistrationPayload()

  // Main device adds delegate to its ApplicationKeys and publishes
  mainApplicationManager.addDevice(payload)
  await mainApplicationManager.publish()

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
  applicationManagers.clear()
  applicationManagerStorages.clear()
  delegateStorages.clear()
}
