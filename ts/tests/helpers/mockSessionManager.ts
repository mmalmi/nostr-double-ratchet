import { vi } from "vitest"
import { OwnerDeviceManager, DelegateDeviceManager } from "../../src/DeviceManager"
import {
  Filter,
  generateSecretKey,
  getPublicKey,
  UnsignedEvent,
  VerifiedEvent,
} from "nostr-tools"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { MockRelay } from "./mockRelay"

export const createMockSessionManager = async (
  deviceId: string,
  sharedMockRelay?: MockRelay,
  existingSecretKey?: Uint8Array,
  existingStorage?: InMemoryStorageAdapter
) => {
  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockStorage = existingStorage || new InMemoryStorageAdapter()
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

  // Create DeviceManager first to handle InviteList
  const deviceManager = new OwnerDeviceManager({
    ownerPublicKey: publicKey,
    identityKey: secretKey,
    deviceId,
    deviceLabel: deviceId,
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })

  await deviceManager.init()

  // Use DeviceManager to create properly configured SessionManager
  const manager = deviceManager.createSessionManager()
  await manager.init()

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    deviceManager,
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
  mainDeviceManager: OwnerDeviceManager
) => {
  const mockStorage = new InMemoryStorageAdapter()
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  // Create subscribe/publish functions that don't need signing (delegate uses its own keys)
  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return sharedMockRelay.subscribe(filter, onEvent)
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    // Delegate's SessionManager signs its own events, so we might receive already-signed events
    if ('sig' in event && event.sig) {
      // Already signed, just add to relay
      const verifiedEvent = event as VerifiedEvent
      // Manually add to relay's events array since we bypass the normal publish flow
      ;(sharedMockRelay as any).events.push(verifiedEvent)
      for (const sub of (sharedMockRelay as any).subscribers.values()) {
        ;(sharedMockRelay as any).deliverToSubscriber(sub, verifiedEvent)
      }
      return verifiedEvent
    }
    // Unsigned event - this shouldn't happen for delegate but handle gracefully
    throw new Error("Delegate publish received unsigned event")
  })

  // Create delegate DeviceManager
  const { manager: delegateDeviceManager, payload } = DelegateDeviceManager.create({
    deviceId,
    deviceLabel: deviceId,
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })

  await delegateDeviceManager.init()

  // Main device adds delegate to its InviteList
  await mainDeviceManager.addDevice(payload)

  // Delegate waits for activation
  await delegateDeviceManager.waitForActivation(5000)

  // Use DeviceManager to create properly configured SessionManager
  const manager = delegateDeviceManager.createSessionManager()
  await manager.init()

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    delegateDeviceManager,
    subscribe,
    publish,
    onEvent,
    mockStorage,
    storageSpy,
    publicKey: delegateDeviceManager.getIdentityPublicKey(),
    relay: sharedMockRelay,
  }
}
