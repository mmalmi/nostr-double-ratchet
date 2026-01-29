import { describe, it, expect, vi, beforeEach } from 'vitest'
import { generateSecretKey, getPublicKey, finalizeEvent, UnsignedEvent, VerifiedEvent } from 'nostr-tools'
import { ApplicationManager, DelegateManager } from '../src/ApplicationManager'
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
    // 1. Create ApplicationManager (authority) - new API only needs nostrPublish
    const applicationManager = new ApplicationManager({
      nostrPublish: createPublish(ownerPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await applicationManager.init()

    // 2. Create DelegateManager for main device (same flow as any device!)
    // Use a holder object to capture the manager reference for the publish closure
    const managerHolder: { manager: DelegateManager | null } = { manager: null }
    const delegatePublish = vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      if ('sig' in event && event.sig) {
        await relay.publishAndDeliver(event as UnsignedEvent)
        return event
      }
      // Get key from manager (available after keys are generated during init)
      const privKey = managerHolder.manager?.getIdentityKey()
      if (!privKey) throw new Error('No delegate key')
      const signedEvent = finalizeEvent(event, privKey)
      await relay.publishAndDeliver(signedEvent as UnsignedEvent)
      return signedEvent
    })

    const mainDelegateManager = new DelegateManager({
      nostrSubscribe: createSubscribe(),
      nostrPublish: delegatePublish,
      storage: new InMemoryStorageAdapter(),
    })
    managerHolder.manager = mainDelegateManager

    await mainDelegateManager.init()
    const mainPayload = mainDelegateManager.getRegistrationPayload()

    // 3. Add main device to ApplicationKeys (local) and publish
    applicationManager.addDevice(mainPayload)
    await applicationManager.publish()

    const devices = applicationManager.getOwnDevices()
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
    // Setup: Create owner's ApplicationManager - new API only needs nostrPublish
    const applicationManager = new ApplicationManager({
      nostrPublish: createPublish(ownerPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await applicationManager.init()

    // Create delegate DelegateManager
    const managerHolder: { manager: DelegateManager | null } = { manager: null }
    const delegatePublish = vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      if ('sig' in event && event.sig) {
        await relay.publishAndDeliver(event as UnsignedEvent)
        return event
      }
      const privKey = managerHolder.manager?.getIdentityKey()
      if (!privKey) throw new Error('No delegate key')
      const signedEvent = finalizeEvent(event, privKey)
      await relay.publishAndDeliver(signedEvent as UnsignedEvent)
      return signedEvent
    })

    const delegateManager = new DelegateManager({
      nostrSubscribe: createSubscribe(),
      nostrPublish: delegatePublish,
      storage: new InMemoryStorageAdapter(),
    })
    managerHolder.manager = delegateManager

    await delegateManager.init()
    const payload = delegateManager.getRegistrationPayload()

    // Start waiting for activation BEFORE adding to ApplicationKeys
    const activationPromise = delegateManager.waitForActivation(5000)

    // Owner adds delegate device (local) and publishes
    applicationManager.addDevice(payload)
    await applicationManager.publish()

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
    const applicationManager = new ApplicationManager({
      nostrPublish: createPublish(ownerPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await applicationManager.init()

    const managerHolder: { manager: DelegateManager | null } = { manager: null }
    const delegatePublish = vi.fn(async (event: UnsignedEvent | VerifiedEvent) => {
      if ('sig' in event && event.sig) {
        await relay.publishAndDeliver(event as UnsignedEvent)
        return event
      }
      const privKey = managerHolder.manager?.getIdentityKey()
      if (!privKey) throw new Error('No delegate key')
      const signedEvent = finalizeEvent(event, privKey)
      await relay.publishAndDeliver(signedEvent as UnsignedEvent)
      return signedEvent
    })

    const delegateManager = new DelegateManager({
      nostrSubscribe: createSubscribe(),
      nostrPublish: delegatePublish,
      storage: new InMemoryStorageAdapter(),
    })
    managerHolder.manager = delegateManager

    await delegateManager.init()
    const payload = delegateManager.getRegistrationPayload()

    // Add and activate device
    const activationPromise = delegateManager.waitForActivation(5000)
    applicationManager.addDevice(payload)
    await applicationManager.publish()
    await activationPromise

    // Device is not revoked initially
    const initialRevoked = await delegateManager.isRevoked()
    expect(initialRevoked).toBe(false)

    // Revoke device and publish
    applicationManager.revokeDevice(payload.identityPubkey)
    await applicationManager.publish()

    // Device detects revocation
    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })
})
