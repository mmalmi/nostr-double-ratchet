import { vi } from "vitest"
import { SessionManager } from "../../src/SessionManager"
import { DeviceManager } from "../../src/DeviceManager"
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
  const deviceManager = DeviceManager.createMain({
    ownerPublicKey: publicKey,
    ownerPrivateKey: secretKey,
    deviceId,
    deviceLabel: deviceId,
    nostrSubscribe: subscribe,
    nostrPublish: publish,
    storage: mockStorage,
  })

  await deviceManager.init()

  // Get ephemeral keypair and shared secret from DeviceManager
  const ephemeralKeypair = deviceManager.getEphemeralKeypair()
  const sharedSecret = deviceManager.getSharedSecret()

  // Create SessionManager with ephemeral keypair for invite response listening
  const manager = new SessionManager(
    publicKey,
    secretKey,
    deviceId,
    subscribe,
    publish,
    mockStorage,
    ephemeralKeypair ?? undefined,
    sharedSecret ?? undefined
  )

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
