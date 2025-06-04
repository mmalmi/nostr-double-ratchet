import { describe, it, expect, vi } from 'vitest'
import { Invite } from '../src/Invite'
import { generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'
import { Session } from '../src/Session'

describe('MultiDevice Communication', () => {
  it('should allow 2 users with 2 devices each to communicate via invites', async () => {
    const aliceKey = generateSecretKey()
    const bobKey = generateSecretKey()
    
    const alicePubKey = getPublicKey(aliceKey)
    const bobPubKey = getPublicKey(bobKey)

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

    const aliceInvite1 = Invite.createNew(alicePubKey, 'Alice Device 1')
    const aliceInvite2 = Invite.createNew(alicePubKey, 'Alice Device 2')
    const bobInvite1 = Invite.createNew(bobPubKey, 'Bob Device 1')
    const bobInvite2 = Invite.createNew(bobPubKey, 'Bob Device 2')

    const sessions: { [key: string]: Session[] } = {
      alice1: [],
      alice2: [],
      bob1: [],
      bob2: []
    }

    const receivedMessages: { [key: string]: any[] } = {
      alice1: [],
      alice2: [],
      bob1: [],
      bob2: []
    }

    aliceInvite1.listen(aliceKey, createSubscribe('Alice1'), (session, identity) => {
      sessions.alice1.push(session)
      session.onEvent((event) => receivedMessages.alice1.push(event))
    })

    aliceInvite2.listen(aliceKey, createSubscribe('Alice2'), (session, identity) => {
      sessions.alice2.push(session)
      session.onEvent((event) => receivedMessages.alice2.push(event))
    })

    bobInvite1.listen(bobKey, createSubscribe('Bob1'), (session, identity) => {
      sessions.bob1.push(session)
      session.onEvent((event) => receivedMessages.bob1.push(event))
    })

    bobInvite2.listen(bobKey, createSubscribe('Bob2'), (session, identity) => {
      sessions.bob2.push(session)
      session.onEvent((event) => receivedMessages.bob2.push(event))
    })

    // Bob devices accept Alice's invites
    const { session: bob1ToAlice1, event: bob1ToAlice1Event } = await bobInvite1.accept(createSubscribe('Bob1ToAlice1'), bobPubKey, bobKey)
    const { session: bob1ToAlice2, event: bob1ToAlice2Event } = await bobInvite2.accept(createSubscribe('Bob1ToAlice2'), bobPubKey, bobKey)
    const { session: bob2ToAlice1, event: bob2ToAlice1Event } = await bobInvite1.accept(createSubscribe('Bob2ToAlice1'), bobPubKey, bobKey)
    const { session: bob2ToAlice2, event: bob2ToAlice2Event } = await bobInvite2.accept(createSubscribe('Bob2ToAlice2'), bobPubKey, bobKey)

    // Alice devices accept Bob's invites
    const { session: alice1ToBob1, event: alice1ToBob1Event } = await aliceInvite1.accept(createSubscribe('Alice1ToBob1'), alicePubKey, aliceKey)
    const { session: alice1ToBob2, event: alice1ToBob2Event } = await aliceInvite2.accept(createSubscribe('Alice1ToBob2'), alicePubKey, aliceKey)
    const { session: alice2ToBob1, event: alice2ToBob1Event } = await aliceInvite1.accept(createSubscribe('Alice2ToBob1'), alicePubKey, aliceKey)
    const { session: alice2ToBob2, event: alice2ToBob2Event } = await aliceInvite2.accept(createSubscribe('Alice2ToBob2'), alicePubKey, aliceKey)

    messageQueue.push(
      bob1ToAlice1Event, bob1ToAlice2Event, bob2ToAlice1Event, bob2ToAlice2Event,
      alice1ToBob1Event, alice1ToBob2Event, alice2ToBob1Event, alice2ToBob2Event
    )

    await new Promise(resolve => setTimeout(resolve, 500))

    bob1ToAlice1.onEvent((event) => receivedMessages.bob1.push(event))
    bob1ToAlice2.onEvent((event) => receivedMessages.bob1.push(event))
    bob2ToAlice1.onEvent((event) => receivedMessages.bob2.push(event))
    bob2ToAlice2.onEvent((event) => receivedMessages.bob2.push(event))
    alice1ToBob1.onEvent((event) => receivedMessages.alice1.push(event))
    alice1ToBob2.onEvent((event) => receivedMessages.alice1.push(event))
    alice2ToBob1.onEvent((event) => receivedMessages.alice2.push(event))
    alice2ToBob2.onEvent((event) => receivedMessages.alice2.push(event))

    messageQueue.push(alice1ToBob1.send('Hello from Alice1').event)
    messageQueue.push(alice2ToBob2.send('Hello from Alice2').event)
    messageQueue.push(bob1ToAlice1.send('Hello from Bob1').event)
    messageQueue.push(bob2ToAlice2.send('Hello from Bob2').event)

    await new Promise(resolve => setTimeout(resolve, 1000))

    expect(receivedMessages.alice1.length).toBeGreaterThan(0)
    expect(receivedMessages.alice2.length).toBeGreaterThan(0)
    expect(receivedMessages.bob1.length).toBeGreaterThan(0)
    expect(receivedMessages.bob2.length).toBeGreaterThan(0)

    const allMessages = [
      ...receivedMessages.alice1,
      ...receivedMessages.alice2,
      ...receivedMessages.bob1,
      ...receivedMessages.bob2
    ]

    expect(allMessages.some(msg => msg.content?.includes('Alice1'))).toBe(true)
    expect(allMessages.some(msg => msg.content?.includes('Alice2'))).toBe(true)
    expect(allMessages.some(msg => msg.content?.includes('Bob1'))).toBe(true)
    expect(allMessages.some(msg => msg.content?.includes('Bob2'))).toBe(true)

    bob1ToAlice1.close()
    bob1ToAlice2.close()
    bob2ToAlice1.close()
    bob2ToAlice2.close()
    alice1ToBob1.close()
    alice1ToBob2.close()
    alice2ToBob1.close()
    alice2ToBob2.close()
  }, 10000)
})
