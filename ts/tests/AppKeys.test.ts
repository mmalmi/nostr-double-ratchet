import { describe, it, expect } from 'vitest'
import { generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools'
import { AppKeys, DeviceEntry } from '../src/AppKeys'
import { APP_KEYS_EVENT_KIND } from '../src/types'

describe('AppKeys', () => {
  /**
   * Create a simple device entry.
   * identityPubkey serves as the device identifier.
   */
  const createTestDevice = (identityPubkey?: string): DeviceEntry => {
    return {
      identityPubkey: identityPubkey || getPublicKey(generateSecretKey()),
      createdAt: Math.floor(Date.now() / 1000),
    }
  }

  describe('constructor and basic properties', () => {
    it('should create an empty AppKeys', () => {
      const list = new AppKeys()

      expect(list.getAllDevices()).toHaveLength(0)
    })

    it('should create AppKeys with initial devices', () => {
      const device = createTestDevice()

      const list = new AppKeys([device])

      expect(list.getAllDevices()).toHaveLength(1)
      expect(list.getAllDevices()[0].identityPubkey).toBe(device.identityPubkey)
    })
  })

  describe('device management', () => {
    it('should add a device', () => {
      const list = new AppKeys()
      const device = createTestDevice()

      list.addDevice(device)

      expect(list.getAllDevices()).toHaveLength(1)
      expect(list.getDevice(device.identityPubkey)).toEqual(device)
    })

    it('should add multiple devices', () => {
      const list = new AppKeys()

      const device1 = createTestDevice()
      const device2 = createTestDevice()
      const device3 = createTestDevice()

      list.addDevice(device1)
      list.addDevice(device2)
      list.addDevice(device3)

      expect(list.getAllDevices()).toHaveLength(3)
    })

    it('should not add duplicate device (same identityPubkey)', () => {
      const list = new AppKeys()
      const device = createTestDevice()

      list.addDevice(device)
      list.addDevice(device) // Add same device again

      expect(list.getAllDevices()).toHaveLength(1)
    })

    it('should remove a device', () => {
      const device = createTestDevice()
      const list = new AppKeys([device])

      expect(list.getAllDevices()).toHaveLength(1)

      list.removeDevice(device.identityPubkey)

      expect(list.getAllDevices()).toHaveLength(0)
      expect(list.getDevice(device.identityPubkey)).toBeUndefined()
    })

    it('should allow re-adding a device after removal', () => {
      const device = createTestDevice()
      const list = new AppKeys([device])

      list.removeDevice(device.identityPubkey)
      expect(list.getAllDevices()).toHaveLength(0)

      list.addDevice(device) // Re-add should work now

      expect(list.getAllDevices()).toHaveLength(1)
      expect(list.getDevice(device.identityPubkey)).toEqual(device)
    })

    it('should get device by identityPubkey', () => {
      const device1 = createTestDevice()
      const device2 = createTestDevice()
      const list = new AppKeys([device1, device2])

      const found = list.getDevice(device2.identityPubkey)

      expect(found).toEqual(device2)
    })

    it('should return undefined for non-existent device', () => {
      const list = new AppKeys()

      expect(list.getDevice('non-existent-pubkey')).toBeUndefined()
    })
  })

  describe('event serialization', () => {
    it('should create a valid unsigned event', () => {
      const device = createTestDevice()
      const list = new AppKeys([device])

      const event = list.getEvent()

      expect(event.kind).toBe(APP_KEYS_EVENT_KIND)
      expect(event.pubkey).toBe('') // Signer will set this
      expect(event.tags).toContainEqual(['d', 'double-ratchet/app-keys'])
      expect(event.tags).toContainEqual(['version', '1'])

      // Simplified tag format: ["device", identityPubkey, createdAt]
      const deviceTag = event.tags.find(t => t[0] === 'device' && t[1] === device.identityPubkey)
      expect(deviceTag).toBeDefined()
      expect(deviceTag!.length).toBe(3)
      expect(deviceTag![1]).toBe(device.identityPubkey)
      expect(deviceTag![2]).toBe(String(device.createdAt))
    })

    it('should not include removed tags in event (devices are simply deleted)', () => {
      const device = createTestDevice()
      const list = new AppKeys([device])

      list.removeDevice(device.identityPubkey)
      const event = list.getEvent()

      // No "removed" tags - device is simply not in the list
      const removedTag = event.tags.find(t => t[0] === 'removed')
      expect(removedTag).toBeUndefined()
      expect(event.tags.filter(t => t[0] === 'device')).toHaveLength(0)
    })

    it('should parse AppKeys from event', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const device = createTestDevice()
      const list = new AppKeys([device])

      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)

      const parsed = AppKeys.fromEvent(signedEvent)

      expect(parsed.getAllDevices()).toHaveLength(1)
      expect(parsed.getAllDevices()[0].identityPubkey).toBe(device.identityPubkey)
      // ownerPublicKey comes from the signed event
      expect(signedEvent.pubkey).toBe(ownerPublicKey)
    })

    it('should parse event after device removal (device is simply gone)', () => {
      const ownerPrivateKey = generateSecretKey()
      const device = createTestDevice()
      const list = new AppKeys([device])
      list.removeDevice(device.identityPubkey)

      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)

      const parsed = AppKeys.fromEvent(signedEvent)

      expect(parsed.getAllDevices()).toHaveLength(0)
    })

    it('should throw on unsigned event', () => {
      const list = new AppKeys()

      const event = list.getEvent()
      // Event without signature
      const unsignedEvent = { ...event, id: 'fake-id', sig: '' } as any

      expect(() => AppKeys.fromEvent(unsignedEvent)).toThrow('Event is not signed')
    })
  })

  describe('serialization for persistence', () => {
    it('should serialize and deserialize', () => {
      const device = createTestDevice()
      const list = new AppKeys([device])

      const json = list.serialize()
      const restored = AppKeys.deserialize(json)

      expect(restored.getAllDevices()).toHaveLength(1)
      expect(restored.getAllDevices()[0].identityPubkey).toBe(device.identityPubkey)
    })

    it('should serialize empty list after device removal', () => {
      const device = createTestDevice()
      const list = new AppKeys([device])
      list.removeDevice(device.identityPubkey)

      const json = list.serialize()
      const restored = AppKeys.deserialize(json)

      expect(restored.getAllDevices()).toHaveLength(0)
    })
  })

  describe('merge (conflict resolution)', () => {
    it('should merge two lists with different devices', () => {
      const device1 = createTestDevice()
      const device2 = createTestDevice()

      const list1 = new AppKeys([device1])
      const list2 = new AppKeys([device2])

      const merged = list1.merge(list2)

      expect(merged.getAllDevices()).toHaveLength(2)
      expect(merged.getDevice(device1.identityPubkey)).toBeDefined()
      expect(merged.getDevice(device2.identityPubkey)).toBeDefined()
    })

    it('should merge two empty lists after removals', () => {
      const device1 = createTestDevice()
      const device2 = createTestDevice()

      const list1 = new AppKeys([device1])
      list1.removeDevice(device1.identityPubkey)

      const list2 = new AppKeys([device2])
      list2.removeDevice(device2.identityPubkey)

      const merged = list1.merge(list2)

      // Both lists are empty after removal, merged is also empty
      expect(merged.getAllDevices()).toHaveLength(0)
    })

    it('should include device from one list when other list has removed it', () => {
      const device = createTestDevice()

      // List1 has the device
      const list1 = new AppKeys([device])

      // List2 has removed the device (so it's empty)
      const list2 = new AppKeys([device])
      list2.removeDevice(device.identityPubkey)

      const merged = list1.merge(list2)

      // Device is in list1, so it appears in merged (no explicit revocation tracking)
      expect(merged.getAllDevices()).toHaveLength(1)
      expect(merged.getDevice(device.identityPubkey)).toBeDefined()
    })

    it('should prefer earlier createdAt during merge for same identityPubkey', () => {
      const identityPubkey = getPublicKey(generateSecretKey())
      const earlierDevice: DeviceEntry = {
        identityPubkey,
        createdAt: 1000,
      }
      const laterDevice: DeviceEntry = {
        identityPubkey,
        createdAt: 2000,
      }

      const list1 = new AppKeys([laterDevice])
      const list2 = new AppKeys([earlierDevice])

      const merged = list1.merge(list2)

      expect(merged.getDevice(identityPubkey)?.createdAt).toBe(1000)
    })
  })

  describe('createDeviceEntry helper', () => {
    it('should create a device entry with identity info', () => {
      const identityPubkey = getPublicKey(generateSecretKey())
      const list = new AppKeys()

      const now = Math.floor(Date.now() / 1000)
      const device = list.createDeviceEntry(identityPubkey)

      expect(device.identityPubkey).toBe(identityPubkey)
      // Allow 1 second tolerance for rounding
      expect(device.createdAt).toBeGreaterThanOrEqual(now - 1)
      expect(device.createdAt).toBeLessThanOrEqual(now + 1)
    })
  })

  describe('DeviceEntry with identityPubkey', () => {
    it('should add device with identityPubkey', () => {
      const list = new AppKeys()

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice(delegatePublicKey)

      list.addDevice(device)

      const retrieved = list.getDevice(device.identityPubkey)
      expect(retrieved).toBeDefined()
      expect(retrieved!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should include identityPubkey in event tags', () => {
      const list = new AppKeys()

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice(delegatePublicKey)

      list.addDevice(device)
      const event = list.getEvent()

      // Simplified format: ["device", identityPubkey, createdAt]
      const deviceTag = event.tags.find(t => t[0] === 'device' && t[1] === device.identityPubkey)
      expect(deviceTag).toBeDefined()
      expect(deviceTag![1]).toBe(delegatePublicKey)
    })

    it('should parse identityPubkey from event', () => {
      const ownerPrivateKey = generateSecretKey()
      const list = new AppKeys()

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice(delegatePublicKey)

      list.addDevice(device)
      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)

      const parsed = AppKeys.fromEvent(signedEvent)
      const parsedDevice = parsed.getDevice(device.identityPubkey)

      expect(parsedDevice).toBeDefined()
      expect(parsedDevice!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should preserve identityPubkey in serialization', () => {
      const list = new AppKeys()

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice(delegatePublicKey)

      list.addDevice(device)
      const json = list.serialize()
      const restored = AppKeys.deserialize(json)

      const restoredDevice = restored.getDevice(device.identityPubkey)
      expect(restoredDevice).toBeDefined()
      expect(restoredDevice!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should preserve identityPubkey in merge', () => {
      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)
      const device = createTestDevice(delegatePublicKey)

      const list1 = new AppKeys([device])
      const list2 = new AppKeys()

      const merged = list1.merge(list2)

      const mergedDevice = merged.getDevice(device.identityPubkey)
      expect(mergedDevice).toBeDefined()
      expect(mergedDevice!.identityPubkey).toBe(delegatePublicKey)
    })

    it('should handle mixed devices (with different identityPubkeys)', () => {
      const ownerPrivateKey = generateSecretKey()
      const ownerPublicKey = getPublicKey(ownerPrivateKey)
      const list = new AppKeys()

      const delegatePrivateKey = generateSecretKey()
      const delegatePublicKey = getPublicKey(delegatePrivateKey)

      const mainDevice = createTestDevice(ownerPublicKey)
      const delegateDevice = createTestDevice(delegatePublicKey)

      list.addDevice(mainDevice)
      list.addDevice(delegateDevice)

      const event = list.getEvent()
      const signedEvent = finalizeEvent(event, ownerPrivateKey)
      const parsed = AppKeys.fromEvent(signedEvent)

      // Both devices should have identityPubkey set correctly
      expect(parsed.getDevice(mainDevice.identityPubkey)?.identityPubkey).toBe(ownerPublicKey)
      expect(parsed.getDevice(delegateDevice.identityPubkey)?.identityPubkey).toBe(delegatePublicKey)
    })
  })
})
