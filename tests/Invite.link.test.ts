import { describe, it, expect } from 'vitest'
import { Invite } from '../src/Invite'
import { generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'
import { Session } from '../src/Session'
import { createEventStream } from '../src/utils'

describe('Invite Link', () => {
  it('should serialize and deserialize invite link correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const label = 'Test Invite'
    const maxUses = 5

    // Create invite
    const invite = Invite.createNew(alicePublicKey, label, maxUses)
    expect(invite.maxUses).toBe(maxUses)
    expect(invite.inviter).toBe(alicePublicKey)
    expect(invite.inviterEphemeralPublicKey).toHaveLength(64)
    expect(invite.sharedSecret).toHaveLength(64)

    // Test URL serialization
    const url = invite.getUrl()
    expect(url).toContain('https://iris.to/#')
    const urlData = JSON.parse(decodeURIComponent(new URL(url).hash.slice(1)))
    expect(urlData.inviter).toBe(alicePublicKey)
    expect(urlData.ephemeralKey).toBe(invite.inviterEphemeralPublicKey)
    expect(urlData.sharedSecret).toBe(invite.sharedSecret)

    // Test URL deserialization
    const parsedInvite = Invite.fromUrl(url)
    expect(parsedInvite.inviter).toBe(alicePublicKey)
    expect(parsedInvite.inviterEphemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
    expect(parsedInvite.sharedSecret).toBe(invite.sharedSecret)
    expect(parsedInvite.maxUses).toBeUndefined() // maxUses is not included in URL
  })

  it('should handle invite link with custom root URL', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'Custom URL Test')

    const customUrl = invite.getUrl('https://custom.example.com')
    expect(customUrl).toContain('https://custom.example.com/#')
    
    const parsedInvite = Invite.fromUrl(customUrl)
    expect(parsedInvite.inviter).toBe(alicePublicKey)
    expect(parsedInvite.inviterEphemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
  })

  it('should throw error for invalid URL', () => {
    expect(() => Invite.fromUrl('https://iris.to/')).toThrow('No invite data found in the URL hash')
    expect(() => Invite.fromUrl('https://iris.to/#invalid')).toThrow('Invite data in URL hash is not valid JSON')
    expect(() => Invite.fromUrl('https://iris.to/#{}')).toThrow('Missing required fields')
  })

  it('should allow communication after serializing and deserializing invite for both parties', async () => {
    // Generate keypairs
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const bobPrivateKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobPrivateKey)

    // Create message queue for testing
    const messageQueue: any[] = []
    const createSubscribe = (name: string) => (filter: any, onEvent: (event: any) => void) => {
      const checkQueue = () => {
        const index = messageQueue.findIndex(event => matchFilter(filter, event))
        if (index !== -1) {
          onEvent(messageQueue.splice(index, 1)[0])
        }
        setTimeout(checkQueue, 100)
      }
      checkQueue()
      return () => {}
    }

    // Step 1: Alice creates invite link
    const invite = Invite.createNew(alicePublicKey, 'Serialized Test')
    const inviteUrl = invite.getUrl()
    console.log('Alice created invite URL:', inviteUrl)

    // Step 2: Bob parses the invite link
    const bobInvite = Invite.fromUrl(inviteUrl)
    console.log('Bob parsed invite URL')

    // Step 3: Set up Alice's session listener
    let aliceSession: Session | undefined
    const aliceSessionPromise = new Promise<Session>((resolve) => {
      invite.listen(
        alicePrivateKey,
        createSubscribe('Alice'),
        (session: Session) => {
          console.log('Alice received session')
          aliceSession = session
          resolve(session)
        }
      )
    })

    // Step 4: Bob accepts the deserialized invite
    console.log('Bob accepting invite...')
    const { session: bobSession, event: acceptanceEvent } = await bobInvite.accept(
      createSubscribe('Bob'),
      bobPublicKey,
      bobPrivateKey
    )
    console.log('Bob created session and acceptance event')

    // Add Bob's acceptance event to the queue
    messageQueue.push(acceptanceEvent)

    // Step 5: Wait for Alice to receive the acceptance and create her session
    await aliceSessionPromise
    expect(aliceSession).toBeDefined()
    console.log('Alice session created successfully')

    // Step 6: Set up message streams
    const aliceMessages = createEventStream(aliceSession!)
    const bobMessages = createEventStream(bobSession)

    // Step 7: Test bidirectional messaging
    console.log('Testing message exchange...')

    // Bob sends first message
    const bobMessage1 = bobSession.send('Hello Alice from Bob!')
    messageQueue.push(bobMessage1.event)
    const aliceReceived1 = await aliceMessages.next()
    expect(aliceReceived1.value?.content).toBe('Hello Alice from Bob!')
    console.log('✓ Alice received Bob\'s message')

    // Alice sends reply
    const aliceMessage1 = aliceSession!.send('Hi Bob from Alice!')
    messageQueue.push(aliceMessage1.event)
    const bobReceived1 = await bobMessages.next()
    expect(bobReceived1.value?.content).toBe('Hi Bob from Alice!')
    console.log('✓ Bob received Alice\'s message')

    // Test another round
    const bobMessage2 = bobSession.send('How are you doing?')
    messageQueue.push(bobMessage2.event)
    const aliceReceived2 = await aliceMessages.next()
    expect(aliceReceived2.value?.content).toBe('How are you doing?')
    console.log('✓ Alice received Bob\'s second message')

    const aliceMessage2 = aliceSession!.send('I\'m doing great, thanks!')
    messageQueue.push(aliceMessage2.event)
    const bobReceived2 = await bobMessages.next()
    expect(bobReceived2.value?.content).toBe('I\'m doing great, thanks!')
    console.log('✓ Bob received Alice\'s second message')

    console.log('✅ All messages exchanged successfully using serialized/deserialized invites')
  })
}) 