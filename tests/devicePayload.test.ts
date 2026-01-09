import { describe, it, expect } from 'vitest'
import { encodeDevicePayload, decodeDevicePayload, DevicePayload } from '../src/inviteUtils'
import { generateEphemeralKeypair, generateSharedSecret, generateDeviceId } from '../src/inviteUtils'

describe('Device Payload Encoding/Decoding', () => {
  const createTestPayload = (label = 'Test Device'): DevicePayload => ({
    ephemeralPubkey: generateEphemeralKeypair().publicKey,
    sharedSecret: generateSharedSecret(),
    deviceId: generateDeviceId(),
    deviceLabel: label,
  })

  describe('encodeDevicePayload', () => {
    it('should encode a device payload to a string', () => {
      const payload = createTestPayload()
      const encoded = encodeDevicePayload(payload)

      expect(typeof encoded).toBe('string')
      expect(encoded.length).toBeGreaterThan(0)
    })

    it('should produce different encodings for different payloads', () => {
      const payload1 = createTestPayload('Device 1')
      const payload2 = createTestPayload('Device 2')

      const encoded1 = encodeDevicePayload(payload1)
      const encoded2 = encodeDevicePayload(payload2)

      expect(encoded1).not.toBe(encoded2)
    })
  })

  describe('decodeDevicePayload', () => {
    it('should decode a valid encoded payload', () => {
      const original = createTestPayload('My Phone')
      const encoded = encodeDevicePayload(original)

      const decoded = decodeDevicePayload(encoded)

      expect(decoded).not.toBeNull()
      expect(decoded!.ephemeralPubkey).toBe(original.ephemeralPubkey)
      expect(decoded!.sharedSecret).toBe(original.sharedSecret)
      expect(decoded!.deviceId).toBe(original.deviceId)
      expect(decoded!.deviceLabel).toBe(original.deviceLabel)
    })

    it('should return null for invalid input', () => {
      expect(decodeDevicePayload('')).toBeNull()
      expect(decodeDevicePayload('not-valid-data')).toBeNull()
      expect(decodeDevicePayload('!!!invalid!!!')).toBeNull()
    })

    it('should return null for truncated input', () => {
      const payload = createTestPayload()
      const encoded = encodeDevicePayload(payload)
      const truncated = encoded.slice(0, encoded.length / 2)

      expect(decodeDevicePayload(truncated)).toBeNull()
    })

    it('should return null for corrupted input', () => {
      const payload = createTestPayload()
      const encoded = encodeDevicePayload(payload)
      // Corrupt a character in the middle
      const corrupted = encoded.slice(0, 10) + 'X' + encoded.slice(11)

      expect(decodeDevicePayload(corrupted)).toBeNull()
    })
  })

  describe('roundtrip', () => {
    it('should roundtrip a simple label', () => {
      const payload = createTestPayload('Phone')
      const encoded = encodeDevicePayload(payload)
      const decoded = decodeDevicePayload(encoded)

      expect(decoded).toEqual(payload)
    })

    it('should roundtrip a label with spaces', () => {
      const payload = createTestPayload('My Work Laptop')
      const encoded = encodeDevicePayload(payload)
      const decoded = decodeDevicePayload(encoded)

      expect(decoded).toEqual(payload)
    })

    it('should roundtrip a label with special characters', () => {
      const payload = createTestPayload('iPhone 15 Pro (Personal)')
      const encoded = encodeDevicePayload(payload)
      const decoded = decodeDevicePayload(encoded)

      expect(decoded).toEqual(payload)
    })

    it('should roundtrip a label with unicode', () => {
      const payload = createTestPayload('我的手机 📱')
      const encoded = encodeDevicePayload(payload)
      const decoded = decodeDevicePayload(encoded)

      expect(decoded).toEqual(payload)
    })

    it('should roundtrip an empty label', () => {
      const payload = createTestPayload('')
      const encoded = encodeDevicePayload(payload)
      const decoded = decodeDevicePayload(encoded)

      expect(decoded).toEqual(payload)
    })
  })

  describe('payload validation', () => {
    it('should reject payload with invalid ephemeralPubkey length', () => {
      const encoded = encodeDevicePayload({
        ephemeralPubkey: 'tooshort',
        sharedSecret: generateSharedSecret(),
        deviceId: generateDeviceId(),
        deviceLabel: 'Test',
      })
      // Even if encoding works, decoding should validate
      const decoded = decodeDevicePayload(encoded)
      expect(decoded).toBeNull()
    })

    it('should reject payload with invalid sharedSecret length', () => {
      const encoded = encodeDevicePayload({
        ephemeralPubkey: generateEphemeralKeypair().publicKey,
        sharedSecret: 'tooshort',
        deviceId: generateDeviceId(),
        deviceLabel: 'Test',
      })
      const decoded = decodeDevicePayload(encoded)
      expect(decoded).toBeNull()
    })

    it('should reject payload with non-hex ephemeralPubkey', () => {
      const encoded = encodeDevicePayload({
        ephemeralPubkey: 'g'.repeat(64), // 'g' is not valid hex
        sharedSecret: generateSharedSecret(),
        deviceId: generateDeviceId(),
        deviceLabel: 'Test',
      })
      const decoded = decodeDevicePayload(encoded)
      expect(decoded).toBeNull()
    })

    it('should reject payload with non-hex sharedSecret', () => {
      const encoded = encodeDevicePayload({
        ephemeralPubkey: generateEphemeralKeypair().publicKey,
        sharedSecret: 'z'.repeat(64), // 'z' is not valid hex
        deviceId: generateDeviceId(),
        deviceLabel: 'Test',
      })
      const decoded = decodeDevicePayload(encoded)
      expect(decoded).toBeNull()
    })
  })

  describe('format properties', () => {
    it('should produce URL-safe output (base64url)', () => {
      const payload = createTestPayload()
      const encoded = encodeDevicePayload(payload)

      // Base64url should not contain +, /, or =
      expect(encoded).not.toMatch(/[+/=]/)
    })

    it('should be reasonably compact', () => {
      const payload = createTestPayload('Phone')
      const encoded = encodeDevicePayload(payload)

      // JSON would be ~180 chars, base64 adds ~33% overhead
      // So we expect roughly 240 chars max for a short label
      expect(encoded.length).toBeLessThan(300)
    })
  })
})
