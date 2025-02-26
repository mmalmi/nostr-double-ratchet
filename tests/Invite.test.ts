import { describe, it, expect, vi } from 'vitest'
import { Invite } from '../src/Invite'
import { finalizeEvent, generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'
import { INVITE_EVENT_KIND, MESSAGE_EVENT_KIND } from '../src/types'
import { Session } from '../src/Session'
import { createEventStream } from '../src/utils'
import { serializeSessionState, deserializeSessionState } from '../src/utils'

describe('Invite', () => {
  const dummySubscribe = vi.fn()

  it('should create a new invite link', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'Test Invite', 5)
    expect(invite.inviterEphemeralPublicKey).toHaveLength(64)
    expect(invite.sharedSecret).toHaveLength(64)
    expect(invite.inviter).toBe(alicePublicKey)
    expect(invite.label).toBe('Test Invite')
    expect(invite.maxUses).toBe(5)
  })

  it('should generate and parse URL correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'Test Invite')
    const url = invite.getUrl()
    const parsedInvite = Invite.fromUrl(url)
    expect(parsedInvite.inviterEphemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
    expect(parsedInvite.sharedSecret).toBe(invite.sharedSecret)
  })

  it('should accept invite and create session', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { session, event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey)

    expect(session).toBeDefined()
    expect(event).toBeDefined()
    expect(event.pubkey).not.toBe(bobPublicKey)
    expect(event.kind).toBe(MESSAGE_EVENT_KIND)
    expect(event.tags).toEqual([['p', invite.inviterEphemeralPublicKey]])
  })

  it('should listen for invite acceptances', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey)

    const onSession = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      expect(filter.kinds).toEqual([MESSAGE_EVENT_KIND])
      expect(filter['#p']).toEqual([invite.inviterEphemeralPublicKey])
      callback(event)
      return () => {}
    }

    invite.listen(
      alicePrivateKey,
      mockSubscribe, 
      onSession
    )

    // Wait for any asynchronous operations to complete
    await new Promise(resolve => setTimeout(resolve, 100))

    expect(onSession).toHaveBeenCalledTimes(1)
    const [session, identity] = onSession.mock.calls[0]
    expect(session).toBeDefined()
    expect(identity).toBe(bobPublicKey)
  })

  it('should allow invitee and inviter to exchange messages', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

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

    let aliceSession: Session | undefined

    const onSession = (session: Session) => {
      aliceSession = session
    }

    invite.listen(
      alicePrivateKey,
      createSubscribe('Alice'),
      onSession
    )

    const { session: bobSession, event } = await invite.accept(createSubscribe('Bob'), bobPublicKey, bobSecretKey)
    messageQueue.push(event)

    // Wait for Alice's session to be created
    await new Promise(resolve => setTimeout(resolve, 100))

    expect(aliceSession).toBeDefined()

    const aliceMessages = createEventStream(aliceSession!)
    const bobMessages = createEventStream(bobSession)

    const sendAndExpect = async (sender: Session, receiver: AsyncIterableIterator<any>, message: string) => {
      messageQueue.push(sender.send(message).event)
      const receivedMessage = await receiver.next()
      expect(receivedMessage.value?.content).toBe(message)
    }

    // Test conversation
    await sendAndExpect(bobSession, aliceMessages, 'Hello Alice!')
    await sendAndExpect(aliceSession!, bobMessages, 'Hi Bob!')
    await sendAndExpect(bobSession, aliceMessages, 'How are you?')
    await sendAndExpect(aliceSession!, bobMessages, "I'm doing great, thanks!")

    // No remaining messages
    expect(messageQueue.length).toBe(0)
  })

  it('should convert between event and Invite correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'Test Invite', 5)
    
    const event = invite.getEvent()
    expect(event.kind).toBe(INVITE_EVENT_KIND)
    expect(event.pubkey).toBe(alicePublicKey)
    expect(event.tags).toContainEqual(['ephemeralKey', invite.inviterEphemeralPublicKey])
    expect(event.tags).toContainEqual(['sharedSecret', invite.sharedSecret])
    expect(event.tags).toContainEqual(['d', 'double-ratchet/invites/public'])
    expect(event.tags).toContainEqual(['l', 'double-ratchet/invites'])

    const finalizedEvent = finalizeEvent(event, alicePrivateKey)
    const parsedInvite = Invite.fromEvent(finalizedEvent)
    
    expect(parsedInvite.inviterEphemeralPublicKey).toBe(invite.inviterEphemeralPublicKey)
    expect(parsedInvite.sharedSecret).toBe(invite.sharedSecret)
    expect(parsedInvite.inviter).toBe(alicePublicKey)
  })

  it('should handle session reinitialization with serialization after invite acceptance', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

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

    let aliceSession: Session | undefined

    const onSession = (session: Session) => {
      aliceSession = session
    }

    invite.listen(
      alicePrivateKey,
      createSubscribe('Alice'),
      onSession
    )

    const { session: bobSession, event } = await invite.accept(createSubscribe('Bob'), bobPublicKey, bobSecretKey)
    messageQueue.push(event)

    // Wait for Alice's session to be created
    await new Promise(resolve => setTimeout(resolve, 100))

    expect(aliceSession).toBeDefined()

    let aliceMessages = createEventStream(aliceSession!)
    const bobMessages = createEventStream(bobSession)

    // Bob sends first message
    messageQueue.push(bobSession.send('Hello Alice!').event)
    const firstMessage = await aliceMessages.next()
    expect(firstMessage.value?.content).toBe('Hello Alice!')

    // Alice closes her session and reinitializes with serialized state
    const serializedAliceState = serializeSessionState(aliceSession!.state)
    aliceSession!.close()
    aliceSession = new Session(createSubscribe('Alice'), deserializeSessionState(serializedAliceState))
    aliceMessages = createEventStream(aliceSession)

    // Bob sends second message
    messageQueue.push(bobSession.send('Can you still hear me?').event)
    const secondMessage = await aliceMessages.next()
    expect(secondMessage.value?.content).toBe('Can you still hear me?')

    // No remaining messages
    expect(messageQueue.length).toBe(0)
  })
})