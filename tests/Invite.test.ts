import { describe, it, expect, vi } from 'vitest'
import { Invite } from '../src/Invite'
import { finalizeEvent, generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'
import { INVITE_EVENT_KIND, MESSAGE_EVENT_KIND } from '../src/types'
import { Channel } from '../src/Channel'
import { createMessageStream } from '../src/utils'

describe('Invite', () => {
  const dummySubscribe = vi.fn()

  it('should create a new invite link', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey, 'Test Invite', 5)
    expect(invite.inviterSessionPublicKey).toHaveLength(64)
    expect(invite.linkSecret).toHaveLength(64)
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
    expect(parsedInvite.inviterSessionPublicKey).toBe(invite.inviterSessionPublicKey)
    expect(parsedInvite.linkSecret).toBe(invite.linkSecret)
  })

  it('should accept invite and create channel', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { channel, event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey)

    expect(channel).toBeDefined()
    expect(event).toBeDefined()
    expect(event.pubkey).not.toBe(bobPublicKey)
    expect(event.kind).toBe(MESSAGE_EVENT_KIND)
    expect(event.tags).toEqual([['p', invite.inviterSessionPublicKey]])
  })

  it('should listen for invite acceptances', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const invite = Invite.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { event } = await invite.accept(dummySubscribe, bobPublicKey, bobSecretKey)

    const onChannel = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      expect(filter.kinds).toEqual([MESSAGE_EVENT_KIND])
      expect(filter['#p']).toEqual([invite.inviterSessionPublicKey])
      callback(event)
      return () => {}
    }

    invite.listen(
      alicePrivateKey,
      mockSubscribe, 
      onChannel
    )

    // Wait for any asynchronous operations to complete
    await new Promise(resolve => setTimeout(resolve, 100))

    expect(onChannel).toHaveBeenCalledTimes(1)
    const [channel, identity] = onChannel.mock.calls[0]
    expect(channel).toBeDefined()
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

    let aliceChannel: Channel | undefined

    const onChannel = (channel: Channel) => {
      aliceChannel = channel
    }

    invite.listen(
      alicePrivateKey,
      createSubscribe('Alice'),
      onChannel
    )

    const { channel: bobChannel, event } = await invite.accept(createSubscribe('Bob'), bobPublicKey, bobSecretKey)
    messageQueue.push(event)

    // Wait for Alice's channel to be created
    await new Promise(resolve => setTimeout(resolve, 100))

    expect(aliceChannel).toBeDefined()

    const aliceMessages = createMessageStream(aliceChannel!)
    const bobMessages = createMessageStream(bobChannel)

    const sendAndExpect = async (sender: Channel, receiver: AsyncIterableIterator<any>, message: string) => {
      messageQueue.push(sender.send(message))
      const receivedMessage = await receiver.next()
      expect(receivedMessage.value?.data).toBe(message)
    }

    // Test conversation
    await sendAndExpect(bobChannel, aliceMessages, 'Hello Alice!')
    await sendAndExpect(aliceChannel!, bobMessages, 'Hi Bob!')
    await sendAndExpect(bobChannel, aliceMessages, 'How are you?')
    await sendAndExpect(aliceChannel!, bobMessages, "I'm doing great, thanks!")

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
    expect(event.tags).toContainEqual(['sessionKey', invite.inviterSessionPublicKey])
    expect(event.tags).toContainEqual(['linkSecret', invite.linkSecret])
    expect(event.tags).toContainEqual(['d', 'nostr-double-ratchet/invite'])

    const finalizedEvent = finalizeEvent(event, alicePrivateKey)
    const parsedInvite = Invite.fromEvent(finalizedEvent)
    
    expect(parsedInvite.inviterSessionPublicKey).toBe(invite.inviterSessionPublicKey)
    expect(parsedInvite.linkSecret).toBe(invite.linkSecret)
    expect(parsedInvite.inviter).toBe(alicePublicKey)
  })
})