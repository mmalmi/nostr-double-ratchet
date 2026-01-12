import { describe, it, expect } from 'vitest'
import { SessionManager } from '../src/SessionManager'

describe('SessionManager.createDelegateDevice', () => {
  it('should generate manager and payload with all required fields', () => {
    const { manager, payload } = SessionManager.createDelegateDevice(
      'device-123',
      'My Delegate Phone',
      () => () => {},
      async () => ({} as any)
    )

    expect(manager).toBeDefined()
    expect(manager.isDelegateMode()).toBe(true)
    expect(payload.deviceId).toBe('device-123')
    expect(payload.deviceLabel).toBe('My Delegate Phone')
    expect(payload.identityPubkey).toBeDefined()
    expect(payload.ephemeralPubkey).toBeDefined()
    expect(payload.sharedSecret).toBeDefined()
  })

  it('should generate valid hex strings for public keys', () => {
    const { payload } = SessionManager.createDelegateDevice(
      'device-123',
      'Test Device',
      () => () => {},
      async () => ({} as any)
    )

    // Public keys should be 64-char hex strings
    expect(payload.identityPubkey).toHaveLength(64)
    expect(payload.identityPubkey).toMatch(/^[0-9a-f]+$/)

    expect(payload.ephemeralPubkey).toHaveLength(64)
    expect(payload.ephemeralPubkey).toMatch(/^[0-9a-f]+$/)

    // Shared secret should be 64-char hex string
    expect(payload.sharedSecret).toHaveLength(64)
    expect(payload.sharedSecret).toMatch(/^[0-9a-f]+$/)
  })

  it('should generate unique keys for each call', () => {
    const { payload: payload1 } = SessionManager.createDelegateDevice(
      'device-1',
      'Device 1',
      () => () => {},
      async () => ({} as any)
    )
    const { payload: payload2 } = SessionManager.createDelegateDevice(
      'device-2',
      'Device 2',
      () => () => {},
      async () => ({} as any)
    )

    expect(payload1.identityPubkey).not.toBe(payload2.identityPubkey)
    expect(payload1.ephemeralPubkey).not.toBe(payload2.ephemeralPubkey)
    expect(payload1.sharedSecret).not.toBe(payload2.sharedSecret)
  })

  it('should use provided deviceId and label', () => {
    const { payload } = SessionManager.createDelegateDevice(
      'custom-device-id',
      'Custom Label 123',
      () => () => {},
      async () => ({} as any)
    )

    expect(payload.deviceId).toBe('custom-device-id')
    expect(payload.deviceLabel).toBe('Custom Label 123')
  })
})
