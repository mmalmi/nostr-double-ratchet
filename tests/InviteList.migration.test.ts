import { describe, it, expect, vi, beforeEach } from 'vitest'
import { generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools'
import { InviteList, DeviceEntry } from '../src/InviteList'
import { Invite } from '../src/Invite'
import { generateEphemeralKeypair, generateSharedSecret, generateDeviceId } from '../src/inviteUtils'
import { INVITE_LIST_EVENT_KIND, INVITE_EVENT_KIND } from '../src/types'
import { MockRelay } from './helpers/mockRelay'

describe('InviteList Migration', () => {
  const createTestDevice = (label?: string): DeviceEntry => {
    const keypair = generateEphemeralKeypair()
    return {
      ephemeralPublicKey: keypair.publicKey,
      ephemeralPrivateKey: keypair.privateKey,
      sharedSecret: generateSharedSecret(),
      deviceId: generateDeviceId(),
      deviceLabel: label || 'Test Device',
      createdAt: Math.floor(Date.now() / 1000),
    }
  }

  describe('migrating from per-device invite to InviteList', () => {
    it('should convert Invite to DeviceEntry format', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'device-1')

      // Extract device entry from invite (simulating migration)
      const deviceEntry: DeviceEntry = {
        ephemeralPublicKey: invite.inviterEphemeralPublicKey,
        ephemeralPrivateKey: invite.inviterEphemeralPrivateKey,
        sharedSecret: invite.sharedSecret,
        deviceId: invite.deviceId!,
        deviceLabel: invite.deviceId!, // Default to deviceId as label
        createdAt: invite.createdAt,
      }

      expect(deviceEntry.ephemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
      expect(deviceEntry.sharedSecret).toBe(invite.sharedSecret)
      expect(deviceEntry.deviceId).toBe('device-1')
    })

    it('should create InviteList from single device invite', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'device-1')

      const deviceEntry: DeviceEntry = {
        ephemeralPublicKey: invite.inviterEphemeralPublicKey,
        ephemeralPrivateKey: invite.inviterEphemeralPrivateKey,
        sharedSecret: invite.sharedSecret,
        deviceId: invite.deviceId!,
        deviceLabel: invite.deviceId!,
        createdAt: invite.createdAt,
      }

      const inviteList = new InviteList(ownerPublicKey, [deviceEntry])

      expect(inviteList.getAllDevices()).toHaveLength(1)
      expect(inviteList.getDevice('device-1')).toBeDefined()
      expect(inviteList.getDevice('device-1')?.ephemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
    })

    it('should merge migrated device into existing InviteList', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      // Device A already migrated
      const deviceA = createTestDevice('Device A')
      const existingList = new InviteList(ownerPublicKey, [deviceA])

      // Device B is migrating
      const inviteB = Invite.createNew(ownerPublicKey, 'device-b')
      const deviceB: DeviceEntry = {
        ephemeralPublicKey: inviteB.inviterEphemeralPublicKey,
        ephemeralPrivateKey: inviteB.inviterEphemeralPrivateKey,
        sharedSecret: inviteB.sharedSecret,
        deviceId: inviteB.deviceId!,
        deviceLabel: inviteB.deviceId!,
        createdAt: inviteB.createdAt,
      }

      // Create local list with just device B
      const localList = new InviteList(ownerPublicKey, [deviceB])

      // Merge (simulating fetch + merge during migration)
      const merged = existingList.merge(localList)

      expect(merged.getAllDevices()).toHaveLength(2)
      expect(merged.getDevice(deviceA.deviceId)).toBeDefined()
      expect(merged.getDevice('device-b')).toBeDefined()
    })
  })

  describe('multi-device migration scenarios', () => {
    it('should handle first device migration (no existing InviteList)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      // Device A migrates first - no InviteList on relay
      const inviteA = Invite.createNew(ownerPublicKey, 'device-a')
      const deviceA: DeviceEntry = {
        ephemeralPublicKey: inviteA.inviterEphemeralPublicKey,
        ephemeralPrivateKey: inviteA.inviterEphemeralPrivateKey,
        sharedSecret: inviteA.sharedSecret,
        deviceId: inviteA.deviceId!,
        deviceLabel: inviteA.deviceId!,
        createdAt: inviteA.createdAt,
      }

      // No remote list exists, so create new one
      const inviteList = new InviteList(ownerPublicKey, [deviceA])

      expect(inviteList.getAllDevices()).toHaveLength(1)
      expect(inviteList.getDevice('device-a')).toBeDefined()
    })

    it('should handle second device migration (merges into existing)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      // Device A already migrated
      const deviceA = createTestDevice('Device A')
      deviceA.deviceId = 'device-a'
      const remoteList = new InviteList(ownerPublicKey, [deviceA])

      // Device B is migrating
      const inviteB = Invite.createNew(ownerPublicKey, 'device-b')
      const deviceB: DeviceEntry = {
        ephemeralPublicKey: inviteB.inviterEphemeralPublicKey,
        ephemeralPrivateKey: inviteB.inviterEphemeralPrivateKey,
        sharedSecret: inviteB.sharedSecret,
        deviceId: 'device-b',
        deviceLabel: 'device-b',
        createdAt: inviteB.createdAt,
      }

      // Device B creates local list and merges with remote
      const localList = new InviteList(ownerPublicKey, [deviceB])
      const merged = remoteList.merge(localList)

      expect(merged.getAllDevices()).toHaveLength(2)
      expect(merged.getDevice('device-a')).toBeDefined()
      expect(merged.getDevice('device-b')).toBeDefined()
    })

    it('should handle race condition (both devices migrate simultaneously)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      // Both devices migrate at the same time, neither sees the other
      const deviceA = createTestDevice('Device A')
      deviceA.deviceId = 'device-a'
      const deviceB = createTestDevice('Device B')
      deviceB.deviceId = 'device-b'

      // Device A creates list with only A
      const listA = new InviteList(ownerPublicKey, [deviceA])
      // Device B creates list with only B (race - didn't see A)
      const listB = new InviteList(ownerPublicKey, [deviceB])

      // Later, on next modification, fetch-merge-publish recovers
      const merged = listA.merge(listB)

      expect(merged.getAllDevices()).toHaveLength(2)
      expect(merged.getDevice('device-a')).toBeDefined()
      expect(merged.getDevice('device-b')).toBeDefined()
    })

    it('should handle offline device migration (merges when comes online)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      // Device A migrated while B was offline
      const deviceA = createTestDevice('Device A')
      deviceA.deviceId = 'device-a'
      const remoteList = new InviteList(ownerPublicKey, [deviceA])

      // Device B comes online and migrates
      const inviteB = Invite.createNew(ownerPublicKey, 'device-b')
      const deviceB: DeviceEntry = {
        ephemeralPublicKey: inviteB.inviterEphemeralPublicKey,
        ephemeralPrivateKey: inviteB.inviterEphemeralPrivateKey,
        sharedSecret: inviteB.sharedSecret,
        deviceId: 'device-b',
        deviceLabel: 'device-b',
        createdAt: inviteB.createdAt,
      }

      // B fetches remote list and merges itself in
      const localList = new InviteList(ownerPublicKey, [deviceB])
      const merged = remoteList.merge(localList)

      expect(merged.getAllDevices()).toHaveLength(2)
    })
  })

  describe('fetch-merge-publish invariant', () => {
    it('should preserve private keys from local during merge', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      // Local device has private key
      const localDevice = createTestDevice('Local')
      const localList = new InviteList(ownerPublicKey, [localDevice])

      // Remote version of same device (from event) has no private key
      const remoteDevice: DeviceEntry = {
        ...localDevice,
        ephemeralPrivateKey: undefined,
      }
      const remoteList = new InviteList(ownerPublicKey, [remoteDevice])

      const merged = localList.merge(remoteList)

      // Private key should be preserved from local
      expect(merged.getDevice(localDevice.deviceId)?.ephemeralPrivateKey).toBeDefined()
    })

    it('should union devices from both lists', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const device1 = createTestDevice('Device 1')
      const device2 = createTestDevice('Device 2')
      const device3 = createTestDevice('Device 3')

      const localList = new InviteList(ownerPublicKey, [device1, device2])
      const remoteList = new InviteList(ownerPublicKey, [device2, device3])

      const merged = localList.merge(remoteList)

      expect(merged.getAllDevices()).toHaveLength(3)
      expect(merged.getDevice(device1.deviceId)).toBeDefined()
      expect(merged.getDevice(device2.deviceId)).toBeDefined()
      expect(merged.getDevice(device3.deviceId)).toBeDefined()
    })

    it('should respect removals from both lists', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const device1 = createTestDevice('Device 1')
      const device2 = createTestDevice('Device 2')

      // Local has device1 removed
      const localList = new InviteList(ownerPublicKey, [device1, device2])
      localList.removeDevice(device1.deviceId)

      // Remote doesn't know about removal
      const remoteList = new InviteList(ownerPublicKey, [device1, device2])

      const merged = localList.merge(remoteList)

      // Device1 should still be removed (removals are permanent)
      expect(merged.getDevice(device1.deviceId)).toBeUndefined()
      expect(merged.getRemovedDeviceIds()).toContain(device1.deviceId)
      expect(merged.getDevice(device2.deviceId)).toBeDefined()
    })
  })

  describe('tombstone for old per-device invite', () => {
    it('should have getDeletionEvent method on Invite', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'device-1')

      // Invite should have a method to create tombstone event
      expect(typeof invite.getDeletionEvent).toBe('function')
    })

    it('should create tombstone event without keys', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'device-1')

      const tombstone = invite.getDeletionEvent()

      expect(tombstone.kind).toBe(INVITE_EVENT_KIND)
      expect(tombstone.pubkey).toBe(ownerPublicKey)
      // Tombstone should have d-tag but no keys
      expect(tombstone.tags.some(t => t[0] === 'd' && t[1].includes('device-1'))).toBe(true)
      expect(tombstone.tags.some(t => t[0] === 'ephemeralKey')).toBe(false)
      expect(tombstone.tags.some(t => t[0] === 'sharedSecret')).toBe(false)
    })
  })

  describe('event kind differences', () => {
    it('should use kind 10078 for InviteList (replaceable)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const event = list.getEvent()

      expect(event.kind).toBe(10078)
      expect(event.kind).toBe(INVITE_LIST_EVENT_KIND)
    })

    it('should use kind 30078 for per-device Invite (addressable)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'device-1')

      const event = invite.getEvent()

      expect(event.kind).toBe(30078)
      expect(event.kind).toBe(INVITE_EVENT_KIND)
    })
  })

  describe('InviteList d-tag', () => {
    it('should use fixed d-tag for InviteList', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const event = list.getEvent()
      const dTag = event.tags.find(t => t[0] === 'd')

      expect(dTag).toBeDefined()
      expect(dTag![1]).toBe('double-ratchet/invite-list')
    })

    it('should use device-specific d-tag for per-device Invite', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'my-device-123')

      const event = invite.getEvent()
      const dTag = event.tags.find(t => t[0] === 'd')

      expect(dTag).toBeDefined()
      expect(dTag![1]).toBe('double-ratchet/invites/my-device-123')
    })
  })
})

describe('Backwards Compatibility', () => {
  describe('reading other users invites', () => {
    it('should be able to use InviteList.accept when InviteList available', async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = {
        ...createTestDevice('Phone'),
        deviceId: 'phone-1',
      }
      const list = new InviteList(ownerPublicKey, [device])

      const inviteePrivateKey = generateSecretKey()
      const inviteePublicKey = getPublicKey(inviteePrivateKey)
      const nostrSubscribe = () => () => {}

      // Accept invite from specific device in InviteList
      const result = await list.accept(
        'phone-1',
        nostrSubscribe,
        inviteePublicKey,
        inviteePrivateKey,
      )

      expect(result.session).toBeDefined()
      expect(result.event).toBeDefined()
    })

    it('should be able to use Invite.accept for legacy per-device invite', async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const invite = Invite.createNew(ownerPublicKey, 'device-1')

      const inviteePrivateKey = generateSecretKey()
      const inviteePublicKey = getPublicKey(inviteePrivateKey)
      const nostrSubscribe = () => () => {}

      const result = await invite.accept(
        nostrSubscribe,
        inviteePublicKey,
        inviteePrivateKey,
      )

      expect(result.session).toBeDefined()
      expect(result.event).toBeDefined()
    })
  })

  describe('device compatibility in accept responses', () => {
    it('should include deviceId in invite response from InviteList.accept', async () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = {
        ...createTestDevice('Phone'),
        deviceId: 'phone-1',
      }
      const list = new InviteList(ownerPublicKey, [device])

      const inviteePrivateKey = generateSecretKey()
      const inviteePublicKey = getPublicKey(inviteePrivateKey)
      const nostrSubscribe = () => () => {}

      const result = await list.accept(
        'phone-1',
        nostrSubscribe,
        inviteePublicKey,
        inviteePrivateKey,
        'invitee-device-1' // invitee's device ID
      )

      // The event should contain the invitee's device ID
      expect(result.event).toBeDefined()
    })
  })
})

function createTestDevice(label: string): DeviceEntry {
  const keypair = generateEphemeralKeypair()
  return {
    ephemeralPublicKey: keypair.publicKey,
    ephemeralPrivateKey: keypair.privateKey,
    sharedSecret: generateSharedSecret(),
    deviceId: generateDeviceId(),
    deviceLabel: label,
    createdAt: Math.floor(Date.now() / 1000),
  }
}
