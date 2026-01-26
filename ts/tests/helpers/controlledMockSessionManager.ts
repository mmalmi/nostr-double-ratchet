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
import { ControlledMockRelay } from "./ControlledMockRelay"

// Store delegate storage and keys for reuse across restarts
const delegateStorages = new Map<string, InMemoryStorageAdapter>()
const delegateKeys = new Map<string, Uint8Array>()

// Store authority DeviceManager per user (only first device is authority)
const authorityDeviceManagers = new Map<string, {
  deviceManager: DeviceManager
  storage: InMemoryStorageAdapter
  publish: ReturnType<typeof vi.fn>
}>()

// Clear all cached state between tests
export const clearControlledMockSessionManagerCache = () => {
  delegateStorages.clear()
  delegateKeys.clear()
  authorityDeviceManagers.clear()
}

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
  const { autoDeliver = true } = options

  const secretKey = existingSecretKey || generateSecretKey()
  const publicKey = getPublicKey(secretKey)

  const mockRelay = sharedMockRelay || new ControlledMockRelay()

  // Check if this user already has an authority device
  const existingAuthority = authorityDeviceManagers.get(publicKey)
  const isAuthority = !existingAuthority

  let deviceManager: DeviceManager | undefined
  let mockStorage: InMemoryStorageAdapter
  let publish: ReturnType<typeof vi.fn>

  if (isAuthority) {
    // First device for this user - becomes authority
    mockStorage = existingStorage || new InMemoryStorageAdapter()

    publish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
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

    deviceManager = new DeviceManager({
      ownerPublicKey: publicKey,
      identityKey: secretKey,
      nostrPublish: publish,
      storage: mockStorage,
      isAuthority: true,
    })

    await deviceManager.init()
    authorityDeviceManagers.set(publicKey, { deviceManager, storage: mockStorage, publish })
  } else {
    // Non-authority device - use existing authority's DeviceManager internally
    mockStorage = existingAuthority.storage
    publish = existingAuthority.publish
    // deviceManager stays undefined - non-authority devices don't have it
  }

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

  const subscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      const handle = mockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  // Create DelegateManager for device identity
  let delegatePrivateKey: Uint8Array | null = null

  const delegateSubscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      const handle = mockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  const delegatePublish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      const verifiedEvent = event as VerifiedEvent
      if (autoDeliver) {
        await mockRelay.publishAndDeliver(verifiedEvent)
      } else {
        ;(mockRelay as any).events.push(verifiedEvent)
      }
      return verifiedEvent
    }
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    if (autoDeliver) {
      await mockRelay.publishAndDeliver(signedEvent)
    } else {
      ;(mockRelay as any).events.push(signedEvent)
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

    const storedOwner = await delegateStorage.get<string>('v3/device-manager/owner-pubkey')
    if (storedOwner) {
      await delegateManager.activate(storedOwner)
    } else {
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
    delegateKeys.set(storageKey, delegatePrivateKey)
    await delegateManager.init()

    // Add device to InviteList using authority's DeviceManager
    const authority = authorityDeviceManagers.get(publicKey)!
    await authority.deviceManager.addDevice(payload)

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
    deviceManager, // undefined for non-authority devices
    delegateManager,
    subscribe,
    publish,
    onEvent,
    mockStorage,
    storageSpy,
    secretKey,
    publicKey,
    relay: mockRelay,
    isAuthority,
  }
}

export const createControlledMockDelegateSessionManager = async (
  deviceId: string,
  sharedMockRelay: ControlledMockRelay,
  mainDeviceManager: DeviceManager,
  options: ControlledMockSessionManagerOptions = {}
) => {
  const { autoDeliver = true } = options

  const mockStorage = new InMemoryStorageAdapter()
  const storageSpy = {
    get: vi.spyOn(mockStorage, "get"),
    del: vi.spyOn(mockStorage, "del"),
    put: vi.spyOn(mockStorage, "put"),
    list: vi.spyOn(mockStorage, "list"),
  }

  let delegatePrivateKey: Uint8Array | null = null

  const delegateSubscribe = vi
    .fn()
    .mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      const handle = sharedMockRelay.subscribe(filter, onEvent)
      return handle.close
    })

  const delegatePublish = vi.fn().mockImplementation(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      const verifiedEvent = event as VerifiedEvent
      if (autoDeliver) {
        await sharedMockRelay.publishAndDeliver(verifiedEvent)
      } else {
        ;(sharedMockRelay as any).events.push(verifiedEvent)
      }
      return verifiedEvent
    }
    if (!delegatePrivateKey) {
      throw new Error("Delegate private key not set yet")
    }
    const signedEvent = finalizeEvent(event, delegatePrivateKey)
    if (autoDeliver) {
      await sharedMockRelay.publishAndDeliver(signedEvent)
    } else {
      ;(sharedMockRelay as any).events.push(signedEvent)
    }
    return signedEvent
  })

  const { manager: delegateManager, payload } = DelegateManager.create({
    nostrSubscribe: delegateSubscribe,
    nostrPublish: delegatePublish,
    storage: mockStorage,
  })

  delegatePrivateKey = delegateManager.getIdentityKey()
  await delegateManager.init()

  // Add device to InviteList
  await mainDeviceManager.addDevice(payload)

  // Wait for activation
  await delegateManager.waitForActivation(5000)

  // Create SessionManager
  const manager = delegateManager.createSessionManager()
  await manager.init()

  const onEvent = vi.fn()
  manager.onEvent(onEvent)

  return {
    manager,
    delegateManager,
    onEvent,
    mockStorage,
    storageSpy,
    relay: sharedMockRelay,
  }
}
