import { describe, it, expect, vi } from 'vitest'
import { InviteLink } from '../src/InviteLink'
import { finalizeEvent, generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'
import { INVITE_EVENT_KIND, MESSAGE_EVENT_KIND } from '../src/types'
import { Channel } from '../src/Channel'
import { createMessageStream } from '../src/utils'

describe('InviteLink', () => {
  const dummySubscribe = vi.fn()

  it('should create a new invite link', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const inviteLink = InviteLink.createNew(alicePublicKey, 'Test Invite', 5)
    expect(inviteLink.inviterSessionPublicKey).toHaveLength(64)
    expect(inviteLink.linkSecret).toHaveLength(64)
    expect(inviteLink.inviter).toBe(alicePublicKey)
    expect(inviteLink.label).toBe('Test Invite')
    expect(inviteLink.maxUses).toBe(5)
  })

  it('should generate and parse URL correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const inviteLink = InviteLink.createNew(alicePublicKey, 'Test Invite')
    const url = inviteLink.getUrl()
    const parsedInviteLink = InviteLink.fromUrl(url)
    expect(parsedInviteLink.inviterSessionPublicKey).toBe(inviteLink.inviterSessionPublicKey)
    expect(parsedInviteLink.linkSecret).toBe(inviteLink.linkSecret)
  })

  it('should accept invite and create channel', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const inviteLink = InviteLink.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { channel, event } = await inviteLink.acceptInvite(dummySubscribe, bobPublicKey, bobSecretKey)

    expect(channel).toBeDefined()
    expect(event).toBeDefined()
    expect(event.pubkey).not.toBe(bobPublicKey)
    expect(event.kind).toBe(MESSAGE_EVENT_KIND)
    expect(event.tags).toEqual([['p', inviteLink.inviterSessionPublicKey]])
  })

  it('should listen for invite acceptances', async () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const inviteLink = InviteLink.createNew(alicePublicKey)
    const bobSecretKey = generateSecretKey()
    const bobPublicKey = getPublicKey(bobSecretKey)

    const { event } = await inviteLink.acceptInvite(dummySubscribe, bobPublicKey, bobSecretKey)

    const onChannel = vi.fn()

    const mockSubscribe = (filter: any, callback: (event: any) => void) => {
      expect(filter.kinds).toEqual([MESSAGE_EVENT_KIND])
      expect(filter['#p']).toEqual([inviteLink.inviterSessionPublicKey])
      callback(event)
      return () => {}
    }

    inviteLink.listen(
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
    const inviteLink = InviteLink.createNew(alicePublicKey)
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

    inviteLink.listen(
      alicePrivateKey,
      createSubscribe('Alice'),
      onChannel
    )

    const { channel: bobChannel, event } = await inviteLink.acceptInvite(createSubscribe('Bob'), bobPublicKey, bobSecretKey)
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

  it('should convert between event and InviteLink correctly', () => {
    const alicePrivateKey = generateSecretKey()
    const alicePublicKey = getPublicKey(alicePrivateKey)
    const inviteLink = InviteLink.createNew(alicePublicKey, 'Test Invite', 5)
    
    const event = inviteLink.getEvent()
    expect(event.kind).toBe(INVITE_EVENT_KIND)
    expect(event.pubkey).toBe(alicePublicKey)
    expect(event.tags).toContainEqual(['sessionKey', inviteLink.inviterSessionPublicKey])
    expect(event.tags).toContainEqual(['linkSecret', inviteLink.linkSecret])
    expect(event.tags).toContainEqual(['d', 'nostr-double-ratchet/invite'])

    const finalizedEvent = finalizeEvent(event, alicePrivateKey)
    const parsedInviteLink = InviteLink.fromEvent(finalizedEvent)
    
    expect(parsedInviteLink.inviterSessionPublicKey).toBe(inviteLink.inviterSessionPublicKey)
    expect(parsedInviteLink.linkSecret).toBe(inviteLink.linkSecret)
    expect(parsedInviteLink.inviter).toBe(alicePublicKey)
  })
})