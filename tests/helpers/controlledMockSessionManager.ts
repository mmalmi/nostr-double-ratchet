import { vi } from "vitest"
import { OwnerDeviceManager, DelegateDeviceManager } from "../../src/DeviceManager"
import {
  Filter,
  finalizeEvent,
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

export const createControlledMockDelegateSessionManager = async (
  deviceId: string,
  sharedMockRelay: ControlledMockRelay,
  mainDeviceManager: OwnerDeviceManager
) => {
  const mockStorage = new InMemoryStorageAdapter()
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  // Context to hold the delegate's private key for signing
  // Will be set after DelegateDeviceManager is created
  let delegatePrivateKey: Uint8Array | null = null

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      const handle = sharedMockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  const publish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    // Already signed - use publishAndDeliver to add directly
    if ('sig' in event && event.sig) {
      const verifiedEvent = event as VerifiedEvent
      await sharedMockRelay.publishAndDeliver(event as UnsignedEvent)
      return verifiedEvent
    }
    // Unsigned event - sign with delegate's private key (for Invite events from DeviceManager)
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    await sharedMockRelay.publishAndDeliver(signedEvent as UnsignedEvent)
    return signedEvent
  })

  // Create delegate DeviceManager
  const { manager: delegateDeviceManager, payload } = DelegateDeviceManager.create({
    deviceId,
    deviceLabel: deviceId,
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })

  // Get the delegate's private key for signing
  delegatePrivateKey = delegateDeviceManager.getIdentityKey() as Uint8Array

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
