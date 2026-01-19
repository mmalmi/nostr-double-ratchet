import { describe, it, expect } from 'vitest'
import { generateSecretKey, getPublicKey, matchFilter, finalizeEvent, UnsignedEvent } from 'nostr-tools'
import { DeviceManager, SessionManager, Rumor, generateDeviceId } from '../src'
import { InMemoryStorageAdapter } from '../src/StorageAdapter'

describe('Delegate Device Messaging', () => {
  // Shared message queue simulating nostr relay (events stay in queue, like a real relay)
  const messageQueue: any[] = []

  const createSubscribe = (_name: string) => (filter: any, onEvent: (event: any) => void) => {
    let unsubscribed = false
    const seenEvents = new Set<string>()

    // Check for matching events (don't remove - events stay on relay)
    const checkQueue = () => {
      if (unsubscribed) return
      for (const event of messageQueue) {
        if (matchFilter(filter, event) && !seenEvents.has(event.id)) {
          seenEvents.add(event.id)
          onEvent(event)
        }
      }
    }

    // Check immediately for existing events
    checkQueue()

    // Set up polling for new events
    const interval = setInterval(() => {
      if (unsubscribed) return
      checkQueue()
    }, 50)

    return () => {
      unsubscribed = true
      clearInterval(interval)
    }
  }

  // Create a publish function that signs events using the provided private key
  // If no key provided, assumes event is already signed
  const createPublish = (_name: string, privateKey?: Uint8Array) => async (event: any) => {
    // If event is already signed, use as-is
    let signedEvent = event
    if (!event.sig && privateKey) {
      // Sign unsigned event
      signedEvent = finalizeEvent(event as UnsignedEvent, privateKey)
    }
    // Avoid duplicates (same event ID)
    if (!messageQueue.some(e => e.id === signedEvent.id)) {
      messageQueue.push(signedEvent)
    }
    return signedEvent
  }

  it('should deliver messages across main and delegate devices for both Alice and Bob', async () => {
    // Clear queue
    messageQueue.length = 0

    // Generate keypairs for Alice and Bob
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const bobPrivateKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobPrivateKey)

    // Track received messages
    const aliceMainMessages: Rumor[] = []
    const aliceDelegateMessages: Rumor[] = []
    const bobMainMessages: Rumor[] = []
    const bobDelegateMessages: Rumor[] = []

    // ============================================================
    // Step 1: Create Alice's main device
    // ============================================================
    console.log('Creating Alice main device...')
    const aliceMainDeviceManager = DeviceManager.createOwnerDevice({
      ownerPublicKey: alicePublicKey,
      ownerPrivateKey: alicePrivateKey,
      deviceId: generateDeviceId(),
      deviceLabel: 'Alice Main',
      nostrSubscribe: createSubscribe('AliceMain'),
      nostrPublish: createPublish('AliceMain', alicePrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await aliceMainDeviceManager.init()

    const aliceMainSessionManager = new SessionManager(
      alicePublicKey,
      alicePrivateKey,
      aliceMainDeviceManager.getDeviceId(),
      createSubscribe('AliceMainSM'),
      createPublish('AliceMainSM', alicePrivateKey),
      alicePublicKey, // ownerPublicKey
      new InMemoryStorageAdapter(),
      aliceMainDeviceManager.getEphemeralKeypair()!,
      aliceMainDeviceManager.getSharedSecret()!
    )
    await aliceMainSessionManager.init()
    aliceMainSessionManager.onEvent((event, from) => {
      console.log(`Alice Main received: "${event.content}" from ${from.slice(0, 8)}`)
      aliceMainMessages.push(event)
    })

    // ============================================================
    // Step 2: Create Alice's delegate device
    // ============================================================
    console.log('Creating Alice delegate device...')
    // Delegate devices don't publish InviteList, so we can use a dummy publish initially
    const { manager: aliceDelegateManager, payload: aliceDelegatePayload } = DeviceManager.createDelegate({
      deviceId: generateDeviceId(),
      deviceLabel: 'Alice Delegate',
      nostrSubscribe: createSubscribe('AliceDelegate'),
      nostrPublish: createPublish('AliceDelegate'), // No signing key needed for delegate DeviceManager
      storage: new InMemoryStorageAdapter(),
    })
    await aliceDelegateManager.init()

    // Start waiting for activation BEFORE main device adds the delegate
    const aliceDelegateActivation = aliceDelegateManager.waitForActivation(5000)

    // Small delay to ensure subscription is set up
    await new Promise(resolve => setTimeout(resolve, 100))

    // Alice main adds delegate to InviteList
    await aliceMainDeviceManager.addDevice(aliceDelegatePayload)
    console.log('Alice main added delegate to InviteList')

    // Wait for activation
    const aliceDelegateOwner = await aliceDelegateActivation
    expect(aliceDelegateOwner).toBe(alicePublicKey)
    console.log('Alice delegate activated')

    // Now get the delegate's identity key for the SessionManager
    const aliceDelegatePrivateKey = aliceDelegateManager.getIdentityPrivateKey()

    // Create SessionManager for Alice delegate
    // NOTE: Delegate must use its OWN public key for DH encryption to work correctly
    // The owner's pubkey is only for UI attribution, not for cryptographic identity
    const aliceDelegatePublicKey = aliceDelegateManager.getIdentityPublicKey()
    const aliceDelegateSessionManager = new SessionManager(
      aliceDelegatePublicKey, // Use delegate's own pubkey for DH encryption
      aliceDelegatePrivateKey,
      aliceDelegateManager.getDeviceId(),
      createSubscribe('AliceDelegateSM'),
      createPublish('AliceDelegateSM'), // Session events are already signed
      alicePublicKey, // ownerPublicKey - delegate belongs to Alice
      new InMemoryStorageAdapter(),
      aliceDelegateManager.getEphemeralKeypair()!,
      aliceDelegateManager.getSharedSecret()!
    )
    await aliceDelegateSessionManager.init()
    aliceDelegateSessionManager.onEvent((event, from) => {
      console.log(`Alice Delegate received: "${event.content}" from ${from.slice(0, 8)}`)
      aliceDelegateMessages.push(event)
    })

    // ============================================================
    // Step 3: Create Bob's main device
    // ============================================================
    console.log('Creating Bob main device...')
    const bobMainDeviceManager = DeviceManager.createOwnerDevice({
      ownerPublicKey: bobPublicKey,
      ownerPrivateKey: bobPrivateKey,
      deviceId: generateDeviceId(),
      deviceLabel: 'Bob Main',
      nostrSubscribe: createSubscribe('BobMain'),
      nostrPublish: createPublish('BobMain', bobPrivateKey),
      storage: new InMemoryStorageAdapter(),
    })
    await bobMainDeviceManager.init()

    const bobMainSessionManager = new SessionManager(
      bobPublicKey,
      bobPrivateKey,
      bobMainDeviceManager.getDeviceId(),
      createSubscribe('BobMainSM'),
      createPublish('BobMainSM', bobPrivateKey),
      bobPublicKey, // ownerPublicKey
      new InMemoryStorageAdapter(),
      bobMainDeviceManager.getEphemeralKeypair()!,
      bobMainDeviceManager.getSharedSecret()!
    )
    await bobMainSessionManager.init()
    bobMainSessionManager.onEvent((event, from) => {
      console.log(`Bob Main received: "${event.content}" from ${from.slice(0, 8)}`)
      bobMainMessages.push(event)
    })

    // ============================================================
    // Step 4: Create Bob's delegate device
    // ============================================================
    console.log('Creating Bob delegate device...')
    const { manager: bobDelegateManager, payload: bobDelegatePayload } = DeviceManager.createDelegate({
      deviceId: generateDeviceId(),
      deviceLabel: 'Bob Delegate',
      nostrSubscribe: createSubscribe('BobDelegate'),
      nostrPublish: createPublish('BobDelegate'), // No signing key needed for delegate DeviceManager
      storage: new InMemoryStorageAdapter(),
    })
    await bobDelegateManager.init()

    // Start waiting for activation BEFORE main device adds the delegate
    const bobDelegateActivation = bobDelegateManager.waitForActivation(5000)

    // Small delay to ensure subscription is set up
    await new Promise(resolve => setTimeout(resolve, 100))

    // Bob main adds delegate to InviteList
    await bobMainDeviceManager.addDevice(bobDelegatePayload)
    console.log('Bob main added delegate to InviteList')

    // Wait for activation
    const bobDelegateOwner = await bobDelegateActivation
    expect(bobDelegateOwner).toBe(bobPublicKey)
    console.log('Bob delegate activated')

    // Now get the delegate's identity key for the SessionManager
    const bobDelegatePrivateKey = bobDelegateManager.getIdentityPrivateKey()

    // Create SessionManager for Bob delegate
    // NOTE: Delegate must use its OWN public key for DH encryption to work correctly
    const bobDelegatePublicKey = bobDelegateManager.getIdentityPublicKey()
    const bobDelegateSessionManager = new SessionManager(
      bobDelegatePublicKey, // Use delegate's own pubkey for DH encryption
      bobDelegatePrivateKey,
      bobDelegateManager.getDeviceId(),
      createSubscribe('BobDelegateSM'),
      createPublish('BobDelegateSM'), // Session events are already signed
      bobPublicKey, // ownerPublicKey - delegate belongs to Bob
      new InMemoryStorageAdapter(),
      bobDelegateManager.getEphemeralKeypair()!,
      bobDelegateManager.getSharedSecret()!
    )
    await bobDelegateSessionManager.init()
    bobDelegateSessionManager.onEvent((event, from) => {
      console.log(`Bob Delegate received: "${event.content}" from ${from.slice(0, 8)}`)
      bobDelegateMessages.push(event)
    })

    // Give time for all subscriptions to set up
    await new Promise(resolve => setTimeout(resolve, 500))

    // ============================================================
    // Step 5: Set up users to establish sessions
    // ============================================================
    console.log('\nSetting up cross-user sessions...')
    aliceMainSessionManager.setupUser(bobPublicKey)
    aliceDelegateSessionManager.setupUser(bobPublicKey)
    bobMainSessionManager.setupUser(alicePublicKey)
    bobDelegateSessionManager.setupUser(alicePublicKey)

    // Wait for sessions to be established
    await new Promise(resolve => setTimeout(resolve, 1000))

    // ============================================================
    // Test 1: Alice main sends message to Bob
    // ============================================================
    console.log('\n--- Test 1: Alice main sends to Bob ---')
    aliceMainMessages.length = 0
    aliceDelegateMessages.length = 0
    bobMainMessages.length = 0
    bobDelegateMessages.length = 0

    await aliceMainSessionManager.sendMessage(bobPublicKey, 'Hello Bob from Alice main!')

    // Wait for message propagation
    await new Promise(resolve => setTimeout(resolve, 1000))

    console.log(`Alice Main received ${aliceMainMessages.length} messages`)
    console.log(`Alice Delegate received ${aliceDelegateMessages.length} messages`)
    console.log(`Bob Main received ${bobMainMessages.length} messages`)
    console.log(`Bob Delegate received ${bobDelegateMessages.length} messages`)

    // Bob's devices should receive the message
    expect(bobMainMessages.length).toBeGreaterThanOrEqual(1)
    expect(bobMainMessages.some(m => m.content === 'Hello Bob from Alice main!')).toBe(true)

    // Alice's own devices should also receive (for sync)
    expect(aliceMainMessages.length + aliceDelegateMessages.length).toBeGreaterThanOrEqual(1)

    // ============================================================
    // Test 2: Alice delegate sends message to Bob
    // ============================================================
    console.log('\n--- Test 2: Alice delegate sends to Bob ---')
    aliceMainMessages.length = 0
    aliceDelegateMessages.length = 0
    bobMainMessages.length = 0
    bobDelegateMessages.length = 0

    await aliceDelegateSessionManager.sendMessage(bobPublicKey, 'Hello Bob from Alice delegate!')

    await new Promise(resolve => setTimeout(resolve, 1000))

    console.log(`Alice Main received ${aliceMainMessages.length} messages`)
    console.log(`Alice Delegate received ${aliceDelegateMessages.length} messages`)
    console.log(`Bob Main received ${bobMainMessages.length} messages`)
    console.log(`Bob Delegate received ${bobDelegateMessages.length} messages`)

    // Bob's devices should receive the message
    expect(bobMainMessages.length + bobDelegateMessages.length).toBeGreaterThanOrEqual(1)

    // ============================================================
    // Test 3: Bob delegate sends message to Alice
    // ============================================================
    console.log('\n--- Test 3: Bob delegate sends to Alice ---')
    aliceMainMessages.length = 0
    aliceDelegateMessages.length = 0
    bobMainMessages.length = 0
    bobDelegateMessages.length = 0

    await bobDelegateSessionManager.sendMessage(alicePublicKey, 'Hello Alice from Bob delegate!')

    await new Promise(resolve => setTimeout(resolve, 1000))

    console.log(`Alice Main received ${aliceMainMessages.length} messages`)
    console.log(`Alice Delegate received ${aliceDelegateMessages.length} messages`)
    console.log(`Bob Main received ${bobMainMessages.length} messages`)
    console.log(`Bob Delegate received ${bobDelegateMessages.length} messages`)

    // Alice's devices should receive the message
    expect(aliceMainMessages.length + aliceDelegateMessages.length).toBeGreaterThanOrEqual(1)

    // ============================================================
    // Cleanup
    // ============================================================
    aliceMainDeviceManager.close()
    aliceDelegateManager.close()
    bobMainDeviceManager.close()
    bobDelegateManager.close()
    aliceMainSessionManager.close()
    aliceDelegateSessionManager.close()
    bobMainSessionManager.close()
    bobDelegateSessionManager.close()

    console.log('\n All tests passed!')
  }, 30000)
})
