import { describe, it, expect, vi, beforeEach } from 'vitest'
import { generateSecretKey, getPublicKey, finalizeEvent, UnsignedEvent, VerifiedEvent } from 'nostr-tools'
import { DeviceManager, DelegateManager } from '../src/DeviceManager'
import { InMemoryStorageAdapter } from '../src/StorageAdapter'
import { ControlledMockRelay } from './helpers/ControlledMockRelay'

describe('Delegate Device Architecture', () => {
  let relay: ControlledMockRelay
  let ownerPrivateKey: Uint8Array
  let ownerPublicKey: string

  beforeEach(() => {
    relay = new ControlledMockRelay()
    ownerPrivateKey = generateSecretKey()
    ownerPublicKey = getPublicKey(ownerPrivateKey)
  })

  const createSubscribe = () => vi.fn((filter, onEvent) => {
    const handle = relay.subscribe(filter, onEvent)
    return handle.close
  })

  const createPublish = (privateKey: Uint8Array) => vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
    if ('sig' in event && event.sig) {
      await relay.publishAndDeliver(event as UnsignedEvent)
      return event
    }
    const signedEvent = finalizeEvent(event, privateKey)
    await relay.publishAndDeliver(signedEvent as UnsignedEvent)
    return signedEvent
  })

  it('main device goes through same pairing flow as delegate device', async () => {
    // 1. Create DeviceManager (authority) - new API only needs nostrPublish
    const deviceManager = new DeviceManager({
      nostrPublish: createPublish(ownerPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await deviceManager.init()

    // 2. Create DelegateManager for main device (same flow as any device!)
    let delegatePrivateKey: Uint8Array | null = null
    const delegatePublish = vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      if ('sig' in event && event.sig) {
        await relay.publishAndDeliver(event as UnsignedEvent)
        return event
      }
      if (!delegatePrivateKey) throw new Error('No delegate key')
      const signedEvent = finalizeEvent(event, delegatePrivateKey)
      await relay.publishAndDeliver(signedEvent as UnsignedEvent)
      return signedEvent
    })

    const { manager: mainDelegateManager, payload: mainPayload } = DelegateManager.create({
      nostrSubscribe: createSubscribe(),
      nostrPublish: delegatePublish,
      storage: new InMemoryStorageAdapter(),
    })

    delegatePrivateKey = mainDelegateManager.getIdentityKey()
    await mainDelegateManager.init()

    // 3. Add main device to InviteList (local) and publish
    deviceManager.addDevice(mainPayload)
    await deviceManager.publish()

    const devices = deviceManager.getOwnDevices()
    expect(devices.length).toBe(1)
    expect(devices[0].identityPubkey).toBe(mainPayload.identityPubkey)

    // 4. Main device identity is separate from owner identity
    expect(mainDelegateManager.getIdentityPublicKey()).not.toBe(ownerPublicKey)

    // 5. Wait for activation
    const activatedOwnerPubkey = await mainDelegateManager.waitForActivation(5000)
    expect(activatedOwnerPubkey).toBe(ownerPublicKey)

    // 6. Create SessionManager
    const sessionManager = mainDelegateManager.createSessionManager()
    expect(sessionManager).toBeDefined()
  })

  it('delegate device can be added and activated', async () => {
    // Setup: Create owner's DeviceManager - new API only needs nostrPublish
    const deviceManager = new DeviceManager({
      nostrPublish: createPublish(ownerPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await deviceManager.init()

    // Create delegate DelegateManager
    let delegatePrivateKey: Uint8Array | null = null
    const delegatePublish = vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      if ('sig' in event && event.sig) {
        await relay.publishAndDeliver(event as UnsignedEvent)
        return event
      }
      if (!delegatePrivateKey) throw new Error('No delegate key')
      const signedEvent = finalizeEvent(event, delegatePrivateKey)
      await relay.publishAndDeliver(signedEvent as UnsignedEvent)
      return signedEvent
    })

    const { manager: delegateManager, payload } = DelegateManager.create({
      nostrSubscribe: createSubscribe(),
      nostrPublish: delegatePublish,
      storage: new InMemoryStorageAdapter(),
    })

    delegatePrivateKey = delegateManager.getIdentityKey()
    await delegateManager.init()

    // Start waiting for activation BEFORE adding to InviteList
    const activationPromise = delegateManager.waitForActivation(5000)

    // Owner adds delegate device (local) and publishes
    deviceManager.addDevice(payload)
    await deviceManager.publish()

    // Delegate receives activation
    const activatedOwnerPubkey = await activationPromise
    expect(activatedOwnerPubkey).toBe(ownerPublicKey)

    // Delegate can create SessionManager
    const sessionManager = delegateManager.createSessionManager()
    expect(sessionManager).toBeDefined()
    expect(delegateManager.getOwnerPublicKey()).toBe(ownerPublicKey)
  })

  it('revoked device is detected', async () => {
    // Setup - new API only needs nostrPublish
    const deviceManager = new DeviceManager({
      nostrPublish: createPublish(ownerPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await deviceManager.init()

    let delegatePrivateKey: Uint8Array | null = null
    const delegatePublish = vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      if ('sig' in event && event.sig) {
        await relay.publishAndDeliver(event as UnsignedEvent)
        return event
      }
      if (!delegatePrivateKey) throw new Error('No delegate key')
      const signedEvent = finalizeEvent(event, delegatePrivateKey)
      await relay.publishAndDeliver(signedEvent as UnsignedEvent)
      return signedEvent
    })

    const { manager: delegateManager, payload } = DelegateManager.create({
      nostrSubscribe: createSubscribe(),
      nostrPublish: delegatePublish,
      storage: new InMemoryStorageAdapter(),
    })

    delegatePrivateKey = delegateManager.getIdentityKey()
    await delegateManager.init()

    // Add and activate device
    const activationPromise = delegateManager.waitForActivation(5000)
    deviceManager.addDevice(payload)
    await deviceManager.publish()
    await activationPromise

    // Device is not revoked initially
    const initialRevoked = await delegateManager.isRevoked()
    expect(initialRevoked).toBe(false)

    // Revoke device and publish
    deviceManager.revokeDevice(payload.identityPubkey)
    await deviceManager.publish()

    // Device detects revocation
    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })
})
