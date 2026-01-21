import { describe, it, expect, vi } from 'vitest'
import { Invite } from '../src/Invite'
import { finalizeEvent, generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'
import { INVITE_EVENT_KIND, INVITE_RESPONSE_KIND } from '../src/types'
import { Session } from '../src/Session'
import { createEventStream } from '../src/utils'
import { serializeSessionState, deserializeSessionState } from '../src/utils'

describe('Invite', () => {
  const dummySubscribe = vi.fn()

  it('should create a new invite link', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'Test Device', 5)
    expect(invite.inviterEphemeralPublicKey).toHaveLength(64)
    expect(invite.sharedSecret).toHaveLength(64)
    expect(invite.inviter).toBe(alicePublicKey)
    expect(invite.deviceId).toBe('Test Device')
    expect(invite.maxUses).toBe(5)
  })

  it('should generate and parse URL correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const url = invite.getUrl()
    const parsedInvite = Invite.fromUrl(url)
    expect(parsedInvite.inviter).toBe(invite.inviter)
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
    expect(event.kind).toBe(INVITE_RESPONSE_KIND)
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
      expect(filter.kinds).toEqual([INVITE_RESPONSE_KIND])
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

  it('should require device ID for getEvent', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    
    expect(() => invite.getEvent()).toThrow('Device ID is required')
  })

  it('should convert between event and Invite correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'test-device')
    
    const event = invite.getEvent()
    expect(event.kind).toBe(INVITE_EVENT_KIND)
    expect(event.pubkey).toBe(alicePublicKey)
    expect(event.tags).toContainEqual(['ephemeralKey', invite.inviterEphemeralPublicKey])
    expect(event.tags).toContainEqual(['sharedSecret', invite.sharedSecret])
    expect(event.tags).toContainEqual(['d', 'double-ratchet/invites/test-device'])
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

  it('should accept invite with deviceId parameter', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { session, event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey, 'device-1')

    expect(session).toBeDefined()
    expect(event).toBeDefined()
    expect(event.pubkey).not.toBe(bobPublicKey)
    expect(event.kind).toBe(INVITE_RESPONSE_KIND)
    expect(event.tags).toEqual([['p', invite.inviterEphemeralPublicKey]])
  })

  it('should pass deviceId to onSession callback', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey, 'device-1')

    const onSession = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      expect(filter.kinds).toEqual([INVITE_RESPONSE_KIND])
      expect(filter['#p']).toEqual([invite.inviterEphemeralPublicKey])
      callback(event)
      return () => {}
    }

    invite.listen(
      alicePrivateKey,
      mockSubscribe, 
      onSession
    )

    await new Promise(resolve => setTimeout(resolve, 100))

    expect(onSession).toHaveBeenCalledTimes(1)
    const [session, identity, deviceId] = onSession.mock.calls[0]
    expect(session).toBeDefined()
    expect(identity).toBe(bobPublicKey)
    expect(deviceId).toBe('device-1')
  })

  it('should use event.id as session name regardless of deviceId', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey, 'device-1')

    const onSession = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      callback(event)
      return () => {}
    }

    invite.listen(
      alicePrivateKey,
      mockSubscribe, 
      onSession
    )

    await new Promise(resolve => setTimeout(resolve, 100))

    expect(onSession).toHaveBeenCalledTimes(1)
    const [session] = onSession.mock.calls[0]
    expect(session.name).toBe(event.id)
  })

  it('should maintain backward compatibility with invites without deviceId', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey)

    const onSession = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      callback(event)
      return () => {}
    }

    invite.listen(
      alicePrivateKey,
      mockSubscribe, 
      onSession
    )

    await new Promise(resolve => setTimeout(resolve, 100))

    expect(onSession).toHaveBeenCalledTimes(1)
    const [session, identity, deviceId] = onSession.mock.calls[0]
    expect(session).toBeDefined()
    expect(identity).toBe(bobPublicKey)
    expect(deviceId).toBeUndefined()
    expect(session.name).toBe(event.id)
  })

  it('should handle mixed old and new format invitations', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)
    const charlieSecretKey = generateSecretKey()
    const charliePublicKey = getPublicKey(charlieSecretKey)

    const { event: bobEvent } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey)
    const { event: charlieEvent } = await invite.accept(dummySubscribe, charliePublicKey, charlieSecretKey, 'device-1')

    const onSession = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      callback(bobEvent)
      callback(charlieEvent)
      return () => {}
    }

    invite.listen(alicePrivateKey, mockSubscribe, onSession)

    await new Promise(resolve => setTimeout(resolve, 100))

    expect(onSession).toHaveBeenCalledTimes(2)

    const calls = onSession.mock.calls
    const bobCall = calls.find(([, identity]) => identity === bobPublicKey)
    const charlieCall = calls.find(([, identity]) => identity === charliePublicKey)

    expect(bobCall[2]).toBeUndefined() // no deviceId
    expect(bobCall[0].name).toBe(bobEvent.id) // session name is event ID

    expect(charlieCall[2]).toBe('device-1') // has deviceId
    expect(charlieCall[0].name).toBe(charlieEvent.id) // session name is event ID
  })

  it('should create valid deletion/tombstone event', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'device-1')
    const tombstone = invite.getDeletionEvent()

    expect(tombstone.kind).toBe(INVITE_EVENT_KIND)
    expect(tombstone.pubkey).toBe(alicePublicKey)

    // Tombstone should have d-tag
    const dTag = tombstone.tags.find(t => t[0] === 'd')
    expect(dTag).toBeDefined()
    expect(dTag![1]).toBe('double-ratchet/invites/device-1')

    // Tombstone should NOT have keys (that's what makes it a tombstone)
    expect(tombstone.tags.some(t => t[0] === 'ephemeralKey')).toBe(false)
    expect(tombstone.tags.some(t => t[0] === 'sharedSecret')).toBe(false)
  })

  it('should require device ID for getDeletionEvent', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey) // no deviceId

    expect(() => invite.getDeletionEvent()).toThrow('Device ID is required')
  })

  describe('Invite Link URL serialization', () => {
    it('should serialize and deserialize invite link correctly', () => {
      const alicePrivateKey = generateSecretKey()
      const alicePublicKey = getPublicKey(alicePrivateKey)
      const label = 'Test Invite'
      const maxUses = 5

      const invite = Invite.createNew(alicePublicKey, label, maxUses)
      expect(invite.maxUses).toBe(maxUses)
      expect(invite.inviter).toBe(alicePublicKey)
      expect(invite.inviterEphemeralPublicKey).toHaveLength(64)
      expect(invite.sharedSecret).toHaveLength(64)

      const url = invite.getUrl()
      expect(url).toContain('https://iris.to/#')
      const urlData = JSON.parse(decodeURIComponent(new URL(url).hash.slice(1)))
      expect(urlData.inviter).toBe(alicePublicKey)
      expect(urlData.ephemeralKey).toBe(invite.inviterEphemeralPublicKey)
      expect(urlData.sharedSecret).toBe(invite.sharedSecret)

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
      const alicePrivateKey = generateSecretKey()
      const alicePublicKey = getPublicKey(alicePrivateKey)
      const bobPrivateKey = generateSecretKey()
      const bobPublicKey = getPublicKey(bobPrivateKey)

      const messageQueue: any[] = []
      const createSubscribe = () => (filter: any, onEvent: (event: any) => void) => {
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

      const invite = Invite.createNew(alicePublicKey, 'Serialized Test')
      const inviteUrl = invite.getUrl()

      const bobInvite = Invite.fromUrl(inviteUrl)

      let aliceSession: Session | undefined
      const aliceSessionPromise = new Promise<Session>((resolve) => {
        invite.listen(
          alicePrivateKey,
          createSubscribe(),
          (session: Session) => {
            aliceSession = session
            resolve(session)
          }
        )
      })

      const { session: bobSession, event: acceptanceEvent } = await bobInvite.accept(
        createSubscribe(),
        bobPublicKey,
        bobPrivateKey
      )

      messageQueue.push(acceptanceEvent)

      await aliceSessionPromise
      expect(aliceSession).toBeDefined()

      const aliceMessages = createEventStream(aliceSession!)
      const bobMessages = createEventStream(bobSession)

      // Bob sends first message
      const bobMessage1 = bobSession.send('Hello Alice from Bob!')
      messageQueue.push(bobMessage1.event)
      const aliceReceived1 = await aliceMessages.next()
      expect(aliceReceived1.value?.content).toBe('Hello Alice from Bob!')

      // Alice sends reply
      const aliceMessage1 = aliceSession!.send('Hi Bob from Alice!')
      messageQueue.push(aliceMessage1.event)
      const bobReceived1 = await bobMessages.next()
      expect(bobReceived1.value?.content).toBe('Hi Bob from Alice!')
    })
  })
})
