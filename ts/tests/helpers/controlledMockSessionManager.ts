import { vi } from "vitest"
import { AppKeysManager, DelegateManager } from "../../src/AppKeysManager"
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
  /** If true, skip calling SessionManager.init() — caller must call it manually */
  skipSessionInit?: boolean
}

export const createControlledMockSessionManager = async (
  deviceId: string,
  sharedMockRelay?: ControlledMockRelay,
  existingSecretKey?: Uint8Array,
  existingStorage?: InMemoryStorageAdapter,
  existingDelegateStorage?: InMemoryStorageAdapter,
  options: ControlledMockSessionManagerOptions = {}
) => {
  const { autoDeliver = true, skipSessionInit = false } = options
  void deviceId // unused but kept for API compatibility

  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockStorage = existingStorage || new InMemoryStorageAdapter()
  const delegateStorage = existingDelegateStorage || new InMemoryStorageAdapter()
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

  // AppKeysManager publish signs with owner's secret key
  const appKeysManagerPublish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
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

  // Create AppKeysManager for AppKeys authority (only needs nostrPublish)
  const appKeysManager = new AppKeysManager({
    nostrPublish: appKeysManagerPublish,
    storage: mockStorage,
  })

  await appKeysManager.init()

  // Create DelegateManager for device identity
  // Use a holder so the publish function can access the manager's key during init
  const managerHolder: { manager: DelegateManager | null } = { manager: null }

  const delegateSubscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      const handle = mockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  const delegatePublish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      const verifiedEvent = event as VerifiedEvent
      await mockRelay.publishAndDeliver(event as UnsignedEvent)
      return verifiedEvent
    }
    const delegatePrivateKey = managerHolder.manager?.getIdentityKey()
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    if (autoDeliver) {
      await mockRelay.publishAndDeliver(signedEvent as UnsignedEvent)
    } else {
      await mockRelay.publish(signedEvent as UnsignedEvent)
    }
    return signedEvent
  })

  const delegateManager = new DelegateManager({
    nostrSubscribe: delegateSubscribe,
    nostrPublish: delegatePublish,
    storage: delegateStorage,
  })
  managerHolder.manager = delegateManager

  await delegateManager.init()
  const payload = delegateManager.getRegistrationPayload()

  // Add device to AppKeys and publish
  appKeysManager.addDevice(payload)
  await appKeysManager.publish() // Publish AppKeys to relay

  // Wait for activation
  await delegateManager.waitForActivation(5000)

  // Create SessionManager using DelegateManager
  const manager = delegateManager.createSessionManager()
  if (!skipSessionInit) {
    await manager.init()
  }

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    appKeysManager,
    delegateManager,
    subscribe,
    publish: appKeysManagerPublish,
    onEvent,
    mockStorage,
    delegateStorage,
    storageSpy,
    secretKey,
    publicKey,
    relay: mockRelay,
  }
}

export const createControlledMockDelegateSessionManager = async (
  _deviceId: string,
  sharedMockRelay: ControlledMockRelay,
  mainAppKeysManager: AppKeysManager
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
    // Unsigned event - sign with delegate's private key
    const delegatePrivateKey = managerHolder.manager?.getIdentityKey()
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    await sharedMockRelay.publishAndDeliver(signedEvent as UnsignedEvent)
    return signedEvent
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

  // Main device adds delegate to its AppKeys and publishes
  mainAppKeysManager.addDevice(payload)
  await mainAppKeysManager.publish()

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
