import { describe, it, expect } from 'vitest'
import { generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools'
import { InviteList, DeviceEntry } from '../src/InviteList'
import { generateDeviceId } from '../src/inviteUtils'
import { INVITE_LIST_EVENT_KIND } from '../src/types'

describe('InviteList', () => {
  /**
   * Create a simple device entry (identity only - no invite crypto).
   * Invite crypto is now in separate Invite events per device.
   */
  const createTestDevice = (label?: string, identityPubkey?: string): DeviceEntry => {
    return {
      deviceId: generateDeviceId(),
      deviceLabel: label || 'Test Device',
      createdAt: Math.floor(Date.now() / 1000),
      identityPubkey: identityPubkey || getPublicKey(generateSecretKey()),
    }
  }

  describe('constructor and basic properties', () => {
    it('should create an empty InviteList', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const list = new InviteList(ownerPublicKey)

      expect(list.ownerPublicKey).toBe(ownerPublicKey)
      expect(list.getAllDevices()).toHaveLength(0)
    })

    it('should create InviteList with initial devices', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('My Phone')

      const list = new InviteList(ownerPublicKey, [device])

      expect(list.getAllDevices()).toHaveLength(1)
      expect(list.getAllDevices()[0].deviceLabel).toBe('My Phone')
    })
  })

  describe('device management', () => {
    it('should add a device', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)
      const device = createTestDevice('Laptop')

      list.addDevice(device)

      expect(list.getAllDevices()).toHaveLength(1)
      expect(list.getDevice(device.deviceId)).toEqual(device)
    })

    it('should add multiple devices', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const device1 = createTestDevice('Phone')
      const device2 = createTestDevice('Laptop')
      const device3 = createTestDevice('Tablet')

      list.addDevice(device1)
      list.addDevice(device2)
      list.addDevice(device3)

      expect(list.getAllDevices()).toHaveLength(3)
    })

    it('should not add duplicate device (same deviceId)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)
      const device = createTestDevice('Phone')

      list.addDevice(device)
      list.addDevice(device) // Add same device again

      expect(list.getAllDevices()).toHaveLength(1)
    })

    it('should remove a device', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      expect(list.getAllDevices()).toHaveLength(1)

      list.removeDevice(device.deviceId)

      expect(list.getAllDevices()).toHaveLength(0)
      expect(list.getDevice(device.deviceId)).toBeUndefined()
    })

    it('should track removed device IDs', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      list.removeDevice(device.deviceId)

      expect(list.getRemovedDeviceIds()).toContain(device.deviceId)
    })

    it('should not allow re-adding a removed device', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      list.removeDevice(device.deviceId)
      list.addDevice(device) // Try to re-add

      expect(list.getAllDevices()).toHaveLength(0)
      expect(list.getDevice(device.deviceId)).toBeUndefined()
    })

    it('should get device by ID', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device1 = createTestDevice('Phone')
      const device2 = createTestDevice('Laptop')
      const list = new InviteList(ownerPublicKey, [device1, device2])

      const found = list.getDevice(device2.deviceId)

      expect(found).toEqual(device2)
    })

    it('should return undefined for non-existent device', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      expect(list.getDevice('non-existent-id')).toBeUndefined()
    })

    it('should update device label', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Old Label')
      const list = new InviteList(ownerPublicKey, [device])

      list.updateDeviceLabel(device.deviceId, 'New Label')

      expect(list.getDevice(device.deviceId)?.deviceLabel).toBe('New Label')
    })
  })

  describe('event serialization', () => {
    it('should create a valid unsigned event', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      const event = list.getEvent()

      expect(event.kind).toBe(INVITE_LIST_EVENT_KIND)
      expect(event.pubkey).toBe(ownerPublicKey)
      expect(event.tags).toContainEqual(['d', 'double-ratchet/invite-list'])
      expect(event.tags).toContainEqual(['version', '2']) // New version for new format

      // New tag format: ["device", deviceId, identityPubkey, createdAt]
      const deviceTag = event.tags.find(t => t[0] === 'device' && t[1] === device.deviceId)
      expect(deviceTag).toBeDefined()
      expect(deviceTag![2]).toBe(device.identityPubkey)
      expect(deviceTag![3]).toBe(String(device.createdAt))
    })

    it('should include removed devices in event tags', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      list.removeDevice(device.deviceId)
      const event = list.getEvent()

      const removedTag = event.tags.find(t => t[0] === 'removed' && t[1] === device.deviceId)
      expect(removedTag).toBeDefined()
    })

    it('should parse InviteList from event', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)

      const parsed = InviteList.fromEvent(signedEvent)

      expect(parsed.ownerPublicKey).toBe(ownerPublicKey)
      expect(parsed.getAllDevices()).toHaveLength(1)
      expect(parsed.getAllDevices()[0].deviceId).toBe(device.deviceId)
      expect(parsed.getAllDevices()[0].identityPubkey).toBe(device.identityPubkey)
      // deviceLabel is no longer published, falls back to deviceId
      expect(parsed.getAllDevices()[0].deviceLabel).toBe(device.deviceId)
    })

    it('should parse removed devices from event', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])
      list.removeDevice(device.deviceId)

      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)

      const parsed = InviteList.fromEvent(signedEvent)

      expect(parsed.getAllDevices()).toHaveLength(0)
      expect(parsed.getRemovedDeviceIds()).toContain(device.deviceId)
    })

    it('should throw on unsigned event', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const event = list.getEvent()
      // Event without signature
      const unsignedEvent = { ...event, id: 'fake-id', sig: '' } as any

      expect(() => InviteList.fromEvent(unsignedEvent)).toThrow('Event is not signed')
    })
  })

  describe('serialization for persistence', () => {
    it('should serialize and deserialize', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])

      const json = list.serialize()
      const restored = InviteList.deserialize(json)

      expect(restored.ownerPublicKey).toBe(ownerPublicKey)
      expect(restored.getAllDevices()).toHaveLength(1)
      expect(restored.getAllDevices()[0].deviceId).toBe(device.deviceId)
      expect(restored.getAllDevices()[0].identityPubkey).toBe(device.identityPubkey)
    })

    it('should preserve removed device IDs in serialization', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice('Phone')
      const list = new InviteList(ownerPublicKey, [device])
      list.removeDevice(device.deviceId)

      const json = list.serialize()
      const restored = InviteList.deserialize(json)

      expect(restored.getRemovedDeviceIds()).toContain(device.deviceId)
    })
  })

  describe('merge (conflict resolution)', () => {
    it('should merge two lists with different devices', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const device1 = createTestDevice('Phone')
      const device2 = createTestDevice('Laptop')

      const list1 = new InviteList(ownerPublicKey, [device1])
      const list2 = new InviteList(ownerPublicKey, [device2])

      const merged = list1.merge(list2)

      expect(merged.getAllDevices()).toHaveLength(2)
      expect(merged.getDevice(device1.deviceId)).toBeDefined()
      expect(merged.getDevice(device2.deviceId)).toBeDefined()
    })

    it('should union removed devices', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const device1 = createTestDevice('Phone')
      const device2 = createTestDevice('Laptop')

      const list1 = new InviteList(ownerPublicKey, [device1])
      list1.removeDevice(device1.deviceId)

      const list2 = new InviteList(ownerPublicKey, [device2])
      list2.removeDevice(device2.deviceId)

      const merged = list1.merge(list2)

      expect(merged.getRemovedDeviceIds()).toContain(device1.deviceId)
      expect(merged.getRemovedDeviceIds()).toContain(device2.deviceId)
    })

    it('should exclude removed devices from active list', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const device = createTestDevice('Phone')

      // List1 has the device
      const list1 = new InviteList(ownerPublicKey, [device])

      // List2 has removed the device
      const list2 = new InviteList(ownerPublicKey, [device])
      list2.removeDevice(device.deviceId)

      const merged = list1.merge(list2)

      // Device should be removed (removal wins)
      expect(merged.getAllDevices()).toHaveLength(0)
      expect(merged.getRemovedDeviceIds()).toContain(device.deviceId)
    })

    it('should prefer earlier createdAt during merge for same deviceId', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const deviceId = generateDeviceId()
      const earlierDevice: DeviceEntry = {
        deviceId,
        deviceLabel: 'Earlier',
        createdAt: 1000,
        identityPubkey: getPublicKey(generateSecretKey()),
      }
      const laterDevice: DeviceEntry = {
        deviceId,
        deviceLabel: 'Later',
        createdAt: 2000,
        identityPubkey: getPublicKey(generateSecretKey()),
      }

      const list1 = new InviteList(ownerPublicKey, [laterDevice])
      const list2 = new InviteList(ownerPublicKey, [earlierDevice])

      const merged = list1.merge(list2)

      expect(merged.getDevice(deviceId)?.createdAt).toBe(1000)
    })
  })

  describe('createDeviceEntry helper', () => {
    it('should create a device entry with identity info', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const identityPubkey = getPublicKey(generateSecretKey())
      const list = new InviteList(ownerPublicKey)

      const now = Math.floor(Date.now() / 1000)
      const device = list.createDeviceEntry('My New Phone', identityPubkey)

      expect(device.deviceId.length).toBeGreaterThan(0)
      expect(device.deviceLabel).toBe('My New Phone')
      expect(device.identityPubkey).toBe(identityPubkey)
      // Allow 1 second tolerance for rounding
      expect(device.createdAt).toBeGreaterThanOrEqual(now - 1)
      expect(device.createdAt).toBeLessThanOrEqual(now + 1)
    })

    it('should allow custom deviceId', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const identityPubkey = getPublicKey(generateSecretKey())
      const list = new InviteList(ownerPublicKey)

      const device = list.createDeviceEntry('My Phone', identityPubkey, 'custom-device-id')

      expect(device.deviceId).toBe('custom-device-id')
    })
  })

  describe('DeviceEntry with identityPubkey (delegate devices)', () => {
    it('should add device with identityPubkey', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice('Delegate Phone', delegatePublicKey)

      list.addDevice(device)

      const retrieved = list.getDevice(device.deviceId)
      expect(retrieved).toBeDefined()
      expect(retrieved!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should include identityPubkey in event tags', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice('Delegate Phone', delegatePublicKey)

      list.addDevice(device)
      const event = list.getEvent()

      // New format: ["device", deviceId, identityPubkey, createdAt]
      const deviceTag = event.tags.find(t => t[0] === 'device' && t[1] === device.deviceId)
      expect(deviceTag).toBeDefined()
      expect(deviceTag![2]).toBe(delegatePublicKey)
    })

    it('should always include identityPubkey in tag', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const device = createTestDevice('Regular Device')
      list.addDevice(device)
      const event = list.getEvent()

      // New format: ["device", deviceId, identityPubkey, createdAt]
      const deviceTag = event.tags.find(t => t[0] === 'device' && t[1] === device.deviceId)
      expect(deviceTag).toBeDefined()
      // Tag should have 4 elements in new format
      expect(deviceTag!.length).toBe(4)
      expect(deviceTag![2]).toBe(device.identityPubkey)
    })

    it('should parse identityPubkey from event', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice('Delegate Phone', delegatePublicKey)

      list.addDevice(device)
      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)

      const parsed = InviteList.fromEvent(signedEvent)
      const parsedDevice = parsed.getDevice(device.deviceId)

      expect(parsedDevice).toBeDefined()
      expect(parsedDevice!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should preserve identityPubkey in serialization', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice('Delegate Phone', delegatePublicKey)

      list.addDevice(device)
      const json = list.serialize()
      const restored = InviteList.deserialize(json)

      const restoredDevice = restored.getDevice(device.deviceId)
      expect(restoredDevice).toBeDefined()
      expect(restoredDevice!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should preserve identityPubkey in merge', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice('Delegate Phone', delegatePublicKey)

      const list1 = new InviteList(ownerPublicKey, [device])
      const list2 = new InviteList(ownerPublicKey)

      const merged = list1.merge(list2)

      const mergedDevice = merged.getDevice(device.deviceId)
      expect(mergedDevice).toBeDefined()
      expect(mergedDevice!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should handle mixed devices (with different identityPubkeys)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new InviteList(ownerPublicKey)

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)

      const mainDevice = createTestDevice('Main Device', ownerPublicKey)
      const delegateDevice = createTestDevice('Delegate Device', delegatePublicKey)

      list.addDevice(mainDevice)
      list.addDevice(delegateDevice)

      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)
      const parsed = InviteList.fromEvent(signedEvent)

      // Both devices should have identityPubkey set correctly
      expect(parsed.getDevice(mainDevice.deviceId)?.identityPubkey).toBe(ownerPublicKey)
      expect(parsed.getDevice(delegateDevice.deviceId)?.identityPubkey).toBe(delegatePublicKey)
    })
  })
})
