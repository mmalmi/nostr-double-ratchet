import { vi } from "vitest"
import { DeviceManager } from "../../src/DeviceManager"
import {
  Filter,
  generateSecretKey,
  getPublicKey,
  UnsignedEvent,
  VerifiedEvent,
} from "nostr-tools"
import { InMemoryStorageAdapter } from "../../src/StorageAdapter"
import { ControlledMockRelay } from "./ControlledMockRelay"

export interface ControlledMockSessionManagerOptions {
  /** If true, auto-deliver events during publish (useful for session setup) */
  autoDeliver?: boolean
}

export const createControlledMockSessionManager = async (
  deviceId: string,
  sharedMockRelay?: ControlledMockRelay,
  existingSecretKey?: Uint8Array,
  existingStorage?: InMemoryStorageAdapter,
  options: ControlledMockSessionManagerOptions = {}
) => {
  const { autoDeliver = true } = options // Default to auto-deliver for easier setup

  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockStorage = existingStorage || new InMemoryStorageAdapter()
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  const mockRelay = sharedMockRelay || new ControlledMockRelay()

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      const handle = mockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
    // Use publishAndDeliver for auto-delivery mode, otherwise just queue
    if (autoDeliver) {
      const eventId = await mockRelay.publishAndDeliver(event, secretKey)
      const allEvents = mockRelay.getAllEvents()
      return allEvents.find((e) => e.id === eventId)
    } else {
      const eventId = await mockRelay.publish(event, secretKey)
      const allEvents = mockRelay.getAllEvents()
      return allEvents.find((e) => e.id === eventId)
    }
  })

  // Create DeviceManager first to handle InviteList
  const deviceManager = DeviceManager.createOwnerDevice({
    ownerPublicKey: publicKey,
    ownerPrivateKey: secretKey,
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

export const createControlledMockDelegateSessionManager = async (
  deviceId: string,
  sharedMockRelay: ControlledMockRelay,
  mainDeviceManager: DeviceManager
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
      const handle = sharedMockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    // Delegate's SessionManager signs its own events, so we might receive already-signed events
    if ('sig' in event && event.sig) {
      // Already signed - use publishAndDeliver to add directly
      // For controlled relay, we need to handle this differently
      const verifiedEvent = event as VerifiedEvent
      await sharedMockRelay.publishAndDeliver(event as UnsignedEvent)
      return verifiedEvent
    }
    // Unsigned event - this shouldn't happen for delegate but handle gracefully
    throw new Error("Delegate publish received unsigned event")
  })

  // Create delegate DeviceManager
  const { manager: delegateDeviceManager, payload } = DeviceManager.createDelegate({
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
