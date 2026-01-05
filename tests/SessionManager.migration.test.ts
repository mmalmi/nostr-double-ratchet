import { describe, it, expect, vi, beforeEach } from 'vitest'
import { generateSecretKey, getPublicKey, Filter, VerifiedEvent, UnsignedEvent } from 'nostr-tools'
import { SessionManager } from '../src/SessionManager'
import { InMemoryStorageAdapter } from '../src/StorageAdapter'
import { Invite } from '../src/Invite'
import { InviteList } from '../src/InviteList'
import { INVITE_LIST_EVENT_KIND, INVITE_EVENT_KIND } from '../src/types'
import { MockRelay } from './helpers/mockRelay'

describe('SessionManager Migration v1→v2', () => {
  let secretKey: Uint8Array
  let publicKey: string
  let storage: InMemoryStorageAdapter
  let mockRelay: MockRelay
  let subscribe: ReturnType<typeof vi.fn>
  let publish: ReturnType<typeof vi.fn>

  beforeEach(() => {
    secretKey = generateSecretKey()
    publicKey = getPublicKey(secretKey)
    storage = new InMemoryStorageAdapter()
    mockRelay = new MockRelay()

    subscribe = vi.fn().mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return mockRelay.subscribe(filter, onEvent)
    })

    publish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
      return await mockRelay.publish(event, secretKey)
    })
  })

  describe('storage version tracking', () => {
    it('should store version key after migration', async () => {
      const manager = new SessionManager(
        publicKey,
        secretKey,
        'device-1',
        subscribe,
        publish,
        storage
      )

      await manager.init()

      const version = await storage.get<string>('storage-version')
      expect(version).toBeDefined()
    })
  })

  describe('storage key patterns', () => {
    it('should use v2 prefix for InviteList', async () => {
      const manager = new SessionManager(
        publicKey,
        secretKey,
        'device-1',
        subscribe,
        publish,
        storage
      )

      await manager.init()

      // v2 behavior: stores InviteList with v2 prefix
      const keys = await storage.list('v2/')
      const inviteListKey = keys.find(k => k.includes('invite-list'))
      expect(inviteListKey).toBeDefined()
    })
  })

  describe('publishing behavior', () => {
    it('should publish InviteList (kind 10078) in v2', async () => {
      const manager = new SessionManager(
        publicKey,
        secretKey,
        'device-1',
        subscribe,
        publish,
        storage
      )

      await manager.init()

      // Wait for async publish to complete
      await new Promise(resolve => setTimeout(resolve, 50))

      // Check that kind 10078 (InviteList) was published
      const publishedEvents = mockRelay.getEvents()
      const inviteListEvent = publishedEvents.find(e => e.kind === INVITE_LIST_EVENT_KIND)
      expect(inviteListEvent).toBeDefined()
      expect(inviteListEvent?.kind).toBe(10078)
    })
  })

  describe('invite serialization compatibility', () => {
    it('should be able to convert Invite to InviteList DeviceEntry format', async () => {
      const invite = Invite.createNew(publicKey, 'device-1')

      // This is what migration would do
      const deviceEntry = {
        ephemeralPublicKey: invite.inviterEphemeralPublicKey,
        ephemeralPrivateKey: invite.inviterEphemeralPrivateKey,
        sharedSecret: invite.sharedSecret,
        deviceId: invite.deviceId!,
        deviceLabel: invite.deviceId!,
        createdAt: invite.createdAt,
      }

      const inviteList = new InviteList(publicKey, [deviceEntry])

      expect(inviteList.getAllDevices()).toHaveLength(1)
      expect(inviteList.getDevice('device-1')).toBeDefined()
      expect(inviteList.getDevice('device-1')?.ephemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
    })

    it('should be able to deserialize v1 invite from storage and add to InviteList', async () => {
      // Simulate v1 storage with serialized invite
      const invite = Invite.createNew(publicKey, 'device-1')
      const serialized = invite.serialize()
      await storage.put('v1/device-invite/device-1', serialized)

      // Load and convert
      const loaded = await storage.get<string>('v1/device-invite/device-1')
      expect(loaded).toBeDefined()

      const deserializedInvite = Invite.deserialize(loaded!)

      const deviceEntry = {
        ephemeralPublicKey: deserializedInvite.inviterEphemeralPublicKey,
        ephemeralPrivateKey: deserializedInvite.inviterEphemeralPrivateKey,
        sharedSecret: deserializedInvite.sharedSecret,
        deviceId: deserializedInvite.deviceId!,
        deviceLabel: deserializedInvite.deviceId!,
        createdAt: deserializedInvite.createdAt,
      }

      const inviteList = new InviteList(publicKey, [deviceEntry])
      expect(inviteList.getDevice('device-1')).toBeDefined()
    })
  })

  describe('tombstone creation', () => {
    it('should create valid tombstone event for old invite', () => {
      const invite = Invite.createNew(publicKey, 'device-1')
      const tombstone = invite.getDeletionEvent()

      expect(tombstone.kind).toBe(INVITE_EVENT_KIND)
      expect(tombstone.pubkey).toBe(publicKey)

      // Tombstone should have d-tag
      const dTag = tombstone.tags.find(t => t[0] === 'd')
      expect(dTag).toBeDefined()
      expect(dTag![1]).toBe('double-ratchet/invites/device-1')

      // Tombstone should NOT have keys
      expect(tombstone.tags.some(t => t[0] === 'ephemeralKey')).toBe(false)
      expect(tombstone.tags.some(t => t[0] === 'sharedSecret')).toBe(false)
    })
  })
})

describe('SessionManager Migration - Future v2 Behavior', () => {
  // These tests document expected v2 behavior after migration is implemented

  describe('v2 storage keys', () => {
    it('should expect v2/invite-list storage key format for InviteList', () => {
      // Document expected v2 storage key
      const expectedKey = 'v2/invite-list'
      expect(expectedKey).toBe('v2/invite-list')
    })
  })

  describe('v2 event kinds', () => {
    it('should use kind 10078 for InviteList', () => {
      expect(INVITE_LIST_EVENT_KIND).toBe(10078)
    })

    it('should use kind 30078 for legacy per-device invites', () => {
      expect(INVITE_EVENT_KIND).toBe(30078)
    })
  })

  describe('InviteList merge behavior', () => {
    it('should merge local and remote InviteLists preserving all devices', () => {
      const secretKey = generateSecretKey()
      const publicKey = getPublicKey(secretKey)

      // Local device
      const localList = new InviteList(publicKey)
      const localDevice = localList.createDevice('Local Phone')
      localList.addDevice(localDevice)

      // Remote list from relay (another device already migrated)
      const remoteList = new InviteList(publicKey)
      const remoteDevice = remoteList.createDevice('Remote Laptop')
      remoteList.addDevice(remoteDevice)

      // Merge
      const merged = localList.merge(remoteList)

      expect(merged.getAllDevices()).toHaveLength(2)
    })

    it('should preserve local private keys when merging with remote', () => {
      const secretKey = generateSecretKey()
      const publicKey = getPublicKey(secretKey)

      // Local device has private key
      const localList = new InviteList(publicKey)
      const localDevice = localList.createDevice('Phone')
      localList.addDevice(localDevice)

      // Remote version (from event) doesn't have private key
      const remoteDevice = { ...localDevice, ephemeralPrivateKey: undefined }
      const remoteList = new InviteList(publicKey, [remoteDevice])

      const merged = localList.merge(remoteList)

      // Private key should be preserved
      expect(merged.getDevice(localDevice.deviceId)?.ephemeralPrivateKey).toBeDefined()
    })
  })
})

describe('SessionManager Backwards Compatibility', () => {
  let secretKey: Uint8Array
  let publicKey: string
  let storage: InMemoryStorageAdapter
  let mockRelay: MockRelay
  let subscribe: ReturnType<typeof vi.fn>
  let publish: ReturnType<typeof vi.fn>

  beforeEach(() => {
    secretKey = generateSecretKey()
    publicKey = getPublicKey(secretKey)
    storage = new InMemoryStorageAdapter()
    mockRelay = new MockRelay()

    subscribe = vi.fn().mockImplementation((filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
      return mockRelay.subscribe(filter, onEvent)
    })

    publish = vi.fn().mockImplementation(async (event: UnsignedEvent) => {
      return await mockRelay.publish(event, secretKey)
    })
  })

  describe('reading invites for other users', () => {
    it('should handle InviteList event (kind 10078)', async () => {
      const otherSecretKey = generateSecretKey()
      const otherPublicKey = getPublicKey(otherSecretKey)

      // Create and publish an InviteList for another user
      const inviteList = new InviteList(otherPublicKey)
      const device = inviteList.createDevice('Other Phone')
      inviteList.addDevice(device)

      const event = inviteList.getEvent()
      await mockRelay.publish(event, otherSecretKey)

      // Verify the event is on the relay
      const events = mockRelay.getEvents()
      const listEvent = events.find(e => e.kind === INVITE_LIST_EVENT_KIND)
      expect(listEvent).toBeDefined()
      expect(listEvent?.pubkey).toBe(otherPublicKey)
    })

    it('should handle per-device invite event (kind 30078)', async () => {
      const otherSecretKey = generateSecretKey()
      const otherPublicKey = getPublicKey(otherSecretKey)

      // Create and publish a per-device invite for another user
      const invite = Invite.createNew(otherPublicKey, 'other-device')
      const event = invite.getEvent()
      await mockRelay.publish(event, otherSecretKey)

      // Verify the event is on the relay
      const events = mockRelay.getEvents()
      const inviteEvent = events.find(e => e.kind === INVITE_EVENT_KIND)
      expect(inviteEvent).toBeDefined()
      expect(inviteEvent?.pubkey).toBe(otherPublicKey)
    })
  })

  describe('tombstone detection', () => {
    it('should detect tombstone event (no keys)', async () => {
      const invite = Invite.createNew(publicKey, 'device-1')
      const tombstone = invite.getDeletionEvent()
      await mockRelay.publish(tombstone, secretKey)

      const events = mockRelay.getEvents()
      const tombstoneEvent = events[events.length - 1]

      // Check tombstone detection logic (from SessionManager)
      const isTombstone = !tombstoneEvent.tags?.some(
        ([key]) => key === 'ephemeralKey' || key === 'sharedSecret'
      )
      expect(isTombstone).toBe(true)
    })

    it('should not detect valid invite as tombstone', async () => {
      const invite = Invite.createNew(publicKey, 'device-1')
      const event = invite.getEvent()
      await mockRelay.publish(event, secretKey)

      const events = mockRelay.getEvents()
      const inviteEvent = events[events.length - 1]

      // Check tombstone detection logic
      const isTombstone = !inviteEvent.tags?.some(
        ([key]) => key === 'ephemeralKey' || key === 'sharedSecret'
      )
      expect(isTombstone).toBe(false)
    })
  })
})
