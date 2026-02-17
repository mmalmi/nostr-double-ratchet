import { describe, it, expect, beforeAll, afterAll } from 'vitest'
import { Invite } from '../src/Invite'
import { Session } from '../src/Session'
import { createEventStream } from '../src/utils'
import { generateSecretKey, getPublicKey } from 'nostr-tools'
import NDK, { NDKEvent, NDKFilter } from '@nostr-dev-kit/ndk'
import { VerifiedEvent } from 'nostr-tools'
import ws from 'ws'

if (typeof global.WebSocket === 'undefined') {
  global.WebSocket = ws
}

// Same relays as iris-client
const DEFAULT_RELAYS = [
  "wss://temp.iris.to",
  "wss://relay.damus.io", 
  "wss://relay.nostr.band",
  "wss://relay.snort.social",
]

// Create NDK subscribe function
const createNdkSubscribe = (ndk: NDK, name: string) => {
  const seenIds = new Set<string>() // deduplicate events across all subscriptions for this participant
  return (filter: NDKFilter, onEvent: (event: VerifiedEvent) => void) => {
    console.log(`${name} subscribing to filter:`, JSON.stringify(filter, null, 2))
    const sub = ndk.subscribe(filter)
    
    sub.on('event', (event: NDKEvent) => {
      if (!event.id || seenIds.has(event.id)) {
        return // skip duplicates
      }
      seenIds.add(event.id)
      console.log(`${name} received event:`, {
        kind: event.kind,
        id: event.id?.substring(0, 8),
        pubkey: event.pubkey?.substring(0, 8),
        tags: event.tags
      })
      onEvent(event as unknown as VerifiedEvent)
    })

    sub.on('eose', () => {
      console.log(`${name} subscription EOSE for filter:`, JSON.stringify(filter, null, 2))
    })

    return () => {
      console.log(`${name} unsubscribing`)
      sub.stop()
    }
  }
}

describe('Invite NDK Integration', () => {
  let aliceNdk: NDK
  let bobNdk: NDK
  let alicePrivateKey: Uint8Array
  let alicePublicKey: string
  let bobPrivateKey: Uint8Array
  let bobPublicKey: string

  beforeAll(async () => {
    // Generate keypairs
    alicePrivateKey = generateSecretKey()
    alicePublicKey = getPublicKey(alicePrivateKey)
    bobPrivateKey = generateSecretKey()
    bobPublicKey = getPublicKey(bobPrivateKey)

    // Initialize NDK instances
    aliceNdk = new NDK({
      explicitRelayUrls: DEFAULT_RELAYS,
      enableOutboxModel: true,
    })
    
    bobNdk = new NDK({
      explicitRelayUrls: DEFAULT_RELAYS,
      enableOutboxModel: true,
    })

    // Connect to relays with longer timeout
    console.log('Connecting to relays...')
    const connectWithTimeout = (ndk: NDK, name: string, ms: number) => {
      return Promise.race([
        ndk.connect(),
        new Promise((_, reject) => setTimeout(() => reject(new Error(`${name} connect() timed out after ${ms}ms`)), ms))
      ])
    }
    try {
      console.log('Connecting Alice NDK...')
      await connectWithTimeout(aliceNdk, 'Alice', 8000)
      console.log('Alice NDK connected (or attempted)')
      console.log('Connecting Bob NDK...')
      await connectWithTimeout(bobNdk, 'Bob', 8000)
      console.log('Bob NDK connected (or attempted)')
      console.log('Initial connection promises resolved')
    } catch (e) {
      console.error('Error during initial connection:', e)
    }

    // Wait for relays to establish connection
    console.log('Waiting for relay connections to stabilize...')
    await new Promise(resolve => setTimeout(resolve, 5000))

    // Check relay connections
    let connectedRelays = 0
    for (const relay of aliceNdk.pool.relays.values()) {
      console.log(`Relay ${relay.url} status:`, relay.status)
      if (relay.status === 1) { // WebSocket.OPEN
        connectedRelays++
        console.log('Connected to relay:', relay.url)
      }
    }
    console.log(`Connected to ${connectedRelays} relays out of ${DEFAULT_RELAYS.length}`)

  }, 30000) // Allow relay connection + stabilization delay

  afterAll(async () => {
    // Clean up connections
    for (const relay of aliceNdk.pool.relays.values()) {
      relay.disconnect()
    }
    for (const relay of bobNdk.pool.relays.values()) {
      relay.disconnect()
    }
  })

  it('should handle invite creation, acceptance, and bidirectional messaging over NDK', async () => {
    console.log('Starting NDK integration test...')
    
    const aliceSubscribe = createNdkSubscribe(aliceNdk, 'Alice')
    const bobSubscribe = createNdkSubscribe(bobNdk, 'Bob')

    // Step 1: Alice creates an invite
    console.log('Alice creating invite...')
    const invite = Invite.createNew(alicePublicKey, 'NDK Test Invite')
    console.log('Invite created with ephemeral key:', invite.inviterEphemeralPublicKey.substring(0, 8))

    // Test invite link serialization/deserialization
    console.log('Testing invite link serialization...')
    const inviteUrl = invite.getUrl()
    console.log('Generated invite URL:', inviteUrl)
    const parsedInvite = Invite.fromUrl(inviteUrl)
    console.log('Parsed invite ephemeral key:', parsedInvite.inviterEphemeralPublicKey.substring(0, 8))
    expect(parsedInvite.inviterEphemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
    expect(parsedInvite.sharedSecret).toBe(invite.sharedSecret)
    expect(parsedInvite.inviter).toBe(invite.inviter)
    console.log('✓ Invite link serialization/deserialization verified')

    let aliceSession: Session | undefined
    const sessionPromise = new Promise<Session>((resolve) => {
      invite.listen(
        alicePrivateKey,
        aliceSubscribe,
        (session: Session, identity?: string) => {
          console.log('Alice received session from identity:', identity?.substring(0, 8))
          aliceSession = session
          resolve(session)
        }
      )
    })

    // Step 2: Bob accepts the invite using the parsed invite
    console.log('Bob accepting invite...')
    const { session: bobSession, event: acceptanceEvent } = await parsedInvite.accept(
      bobSubscribe,
      bobPublicKey,
      bobPrivateKey
    )
    console.log('Bob created session and acceptance event:', {
      id: acceptanceEvent.id?.substring(0, 8),
      kind: acceptanceEvent.kind,
      tags: acceptanceEvent.tags
    })

    // Publish Bob's acceptance event
    const bobAcceptanceNdkEvent = new NDKEvent(bobNdk, acceptanceEvent)
    try {
      await bobAcceptanceNdkEvent.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }
    console.log('Bob published acceptance event')

    // Step 3: Wait for Alice to receive the acceptance and create her session
    console.log('Waiting for Alice to receive acceptance...')
    await sessionPromise
    expect(aliceSession).toBeDefined()
    console.log('Alice session created successfully')

    // Step 4: Set up message streams
    const aliceMessages = createEventStream(aliceSession!)
    const bobMessages = createEventStream(bobSession)

    // Step 5: Test bidirectional messaging
    console.log('Starting message exchange...')

    // Bob sends first message
    console.log('Bob sending first message...')
    const bobMessage1 = bobSession.send('Hello Alice from Bob!')
    const bobEvent1 = new NDKEvent(bobNdk, bobMessage1.event)
    try {
      await bobEvent1.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }
    console.log('Bob published first message:', bobEvent1.id?.substring(0, 8))

    // Alice should receive Bob's message
    console.log('Waiting for Alice to receive Bob\'s message...')
    const aliceReceived1 = await Promise.race([
      aliceMessages.next(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('Timeout waiting for Alice to receive message')), 10000))
    ]) as { value?: { content: string } }
    expect(aliceReceived1.value?.content).toBe('Hello Alice from Bob!')
    console.log('✓ Alice received Bob\'s message:', aliceReceived1.value?.content)

    // Alice sends reply
    console.log('Alice sending reply...')
    const aliceMessage1 = aliceSession!.send('Hi Bob from Alice!')
    const aliceEvent1 = new NDKEvent(aliceNdk, aliceMessage1.event)
    try {
      await aliceEvent1.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }
    console.log('Alice published reply:', aliceEvent1.id?.substring(0, 8))

    // Bob should receive Alice's message
    console.log('Waiting for Bob to receive Alice\'s message...')
    const bobReceived1 = await Promise.race([
      bobMessages.next(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('Timeout waiting for Bob to receive message')), 10000))
    ]) as { value?: { content: string } }
    expect(bobReceived1.value?.content).toBe('Hi Bob from Alice!')
    console.log('✓ Bob received Alice\'s message:', bobReceived1.value?.content)

    // Test another round to ensure continued communication
    console.log('Testing second round of messages...')

    // Bob sends second message
    const bobMessage2 = bobSession.send('How are you doing?')
    const bobEvent2 = new NDKEvent(bobNdk, bobMessage2.event)
    try {
      await bobEvent2.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }
    console.log('Bob published second message')

    const aliceReceived2 = await Promise.race([
      aliceMessages.next(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('Timeout waiting for Alice to receive second message')), 10000))
    ]) as { value?: { content: string } }
    expect(aliceReceived2.value?.content).toBe('How are you doing?')
    console.log('✓ Alice received Bob\'s second message')

    // Alice sends second reply
    const aliceMessage2 = aliceSession!.send('I\'m doing great, thanks!')
    const aliceEvent2 = new NDKEvent(aliceNdk, aliceMessage2.event)
    try {
      await aliceEvent2.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }
    console.log('Alice published second reply')

    const bobReceived2 = await Promise.race([
      bobMessages.next(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('Timeout waiting for Bob to receive second reply')), 10000))
    ]) as { value?: { content: string } }
    expect(bobReceived2.value?.content).toBe('I\'m doing great, thanks!')
    console.log('✓ Bob received Alice\'s second message')

    console.log('✅ All tests passed! Bidirectional messaging works over NDK transport')

  }, 30000) // 30 second timeout for the entire test

  it('should handle session state persistence and message delivery after reconnection', async () => {
    console.log('Testing session persistence and reconnection...')

    // Create a fresh invite and session
    const invite = Invite.createNew(alicePublicKey, 'Persistence Test')
    const aliceSubscribe = createNdkSubscribe(aliceNdk, 'Alice-Persist')
    const bobSubscribe = createNdkSubscribe(bobNdk, 'Bob-Persist')

    let aliceSession: Session | undefined
    const sessionPromise = new Promise<Session>((resolve) => {
      invite.listen(
        alicePrivateKey,
        aliceSubscribe,
        (session: Session) => {
          aliceSession = session
          resolve(session)
        }
      )
    })

    // Bob accepts invite
    const { session: bobSession, event: acceptanceEvent } = await invite.accept(
      bobSubscribe,
      bobPublicKey,
      bobPrivateKey
    )

    const bobAcceptanceNdkEvent = new NDKEvent(bobNdk, acceptanceEvent)
    try {
      await bobAcceptanceNdkEvent.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }

    await sessionPromise
    expect(aliceSession).toBeDefined()

    // Exchange initial messages
    const aliceMessages = createEventStream(aliceSession!)
    const bobMessages = createEventStream(bobSession)

    // Bob sends message
    const bobMessage = bobSession.send('Message before reconnection')
    const bobEvent = new NDKEvent(bobNdk, bobMessage.event)
    try {
      await bobEvent.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }

    const aliceReceived = await aliceMessages.next() as { value?: { content: string } }
    expect(aliceReceived.value?.content).toBe('Message before reconnection')

    // Simulate Alice's session being recreated (like after page refresh)
    const { serializeSessionState, deserializeSessionState } = await import('../src/utils')
    const serializedState = serializeSessionState(aliceSession!.state)
    aliceSession!.close()

    // Recreate Alice's session from serialized state
    const newAliceSession = new Session(aliceSubscribe, deserializeSessionState(serializedState))
    const newAliceMessages = createEventStream(newAliceSession)

    // Bob sends another message
    const bobMessage2 = bobSession.send('Message after Alice reconnection')
    const bobEvent2 = new NDKEvent(bobNdk, bobMessage2.event)
    try {
      await bobEvent2.publish()
    } catch (e) {
      console.log('Ignoring NDK publish error:', e.message)
    }

    // Alice should still receive the message with her new session
    const aliceReceived2 = await Promise.race([
      newAliceMessages.next(),
      new Promise((_, reject) => setTimeout(() => reject(new Error('Timeout after reconnection')), 10000))
    ]) as { value?: { content: string } }
    expect(aliceReceived2.value?.content).toBe('Message after Alice reconnection')

    console.log('✅ Session persistence test passed!')

  }, 20000)
}) 
