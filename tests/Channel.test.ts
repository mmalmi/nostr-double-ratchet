import { describe, it, expect } from 'vitest'
import { Channel } from '../src/Channel'
import { getPublicKey, generateSecretKey, matchFilter } from 'nostr-tools'
import { EVENT_KIND } from '../src/types';
import { createMessageStream } from '../src/utils';

describe('Channel', () => {
  const aliceSecretKey = generateSecretKey()
  const bobSecretKey = generateSecretKey()
  const dummyUnsubscribe = () => {}
  const dummySubscribe = () => dummyUnsubscribe

  it('should initialize with correct properties', () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey)

    expect(alice.state.theirNostrPublicKey).toBe(getPublicKey(bobSecretKey))
    expect(alice.state.ourCurrentNostrKey.publicKey).toBe(getPublicKey(aliceSecretKey))
    expect(alice.state.ourCurrentNostrKey.publicKey).toHaveLength(64) // Hex-encoded public key length
  })

  it('should create an event with correct properties', () => {
    const channel = Channel.init(() => dummyUnsubscribe, getPublicKey(bobSecretKey), aliceSecretKey)
    const testData = 'Hello, world!'

    const event = channel.send(testData)

    expect(event).toBeTruthy()
    expect(event.kind).toBe(EVENT_KIND)
    expect(event.tags[0][0]).toEqual("header")
    expect(event.tags[0][1]).toBeTruthy()
    expect(event.content).toBeTruthy()
    expect(typeof event.created_at).toBe('number')
    expect(event.pubkey).toHaveLength(64)
    expect(event.id).toHaveLength(64)
    expect(event.sig).toHaveLength(128)
  })

  it('should handle incoming events and update keys', async () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey, undefined, 'alice', true)
    const event = alice.send('Hello, Bob!')
    
    const bob = Channel.init((filter, onEvent) => {
      if (matchFilter(filter, event)) {
        onEvent(event)
      }
      return dummyUnsubscribe
    }, getPublicKey(aliceSecretKey), bobSecretKey, undefined, 'bob', false)

    const initialReceivingChainKey = bob.state.receivingChainKey

    const bobMessages = createMessageStream(bob);

    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.data).toBe('Hello, Bob!')

    const nextReceivingChainKey = bob.state.receivingChainKey
    expect(nextReceivingChainKey).not.toBe(initialReceivingChainKey)
  })

  it('should handle multiple back-and-forth messages correctly', async () => {
    const messageQueue: any[] = [];

    const createSubscribe = () => (filter: any, onEvent: (event: any) => void) => {
      const checkQueue = () => {
        const index = messageQueue.findIndex(event => matchFilter(filter, event));
        if (index !== -1) {
          onEvent(messageQueue.splice(index, 1)[0]);
        }
        setTimeout(checkQueue, 100);
      };
      checkQueue();
      return () => {};
    };

    const alice = Channel.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, undefined, 'alice', true);
    const bob = Channel.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, undefined, 'bob', false);

    const aliceMessages = createMessageStream(alice);
    const bobMessages = createMessageStream(bob);

    const sendAndExpect = async (sender: Channel, receiver: AsyncIterableIterator<any>, message: string, receiverChannel: Channel) => {
      messageQueue.push(sender.send(message));
      const receivedMessage = await receiver.next();

      console.log(`${receiverChannel.name} got from ${sender.name}: ${receivedMessage.value.data}`)
      expect(receivedMessage.value?.data).toBe(message);
    };

    // Test conversation
    await sendAndExpect(alice, bobMessages, 'Hello Bob!', bob);
    await sendAndExpect(bob, aliceMessages, 'Hi Alice!', alice);
    await sendAndExpect(alice, bobMessages, 'How are you?', bob);

    // Test consecutive messages from Bob
    await sendAndExpect(bob, aliceMessages, 'I am fine, thank you!', alice);
    await sendAndExpect(bob, aliceMessages, 'How about you?', alice);

    // Final message from Alice
    await sendAndExpect(alice, bobMessages, "I'm doing great, thanks!", bob);

    // No remaining messagess
    expect(messageQueue.length).toBe(0);
  })

  it('should handle out-of-order message delivery correctly', async () => {
    const messageQueue: any[] = [];

    const createSubscribe = () => (filter: any, onEvent: (event: any) => void) => {
      const checkQueue = () => {
        const index = messageQueue.findIndex(event => matchFilter(filter, event));
        if (index !== -1) {
          onEvent(messageQueue.splice(index, 1)[0]);
        }
        setTimeout(checkQueue, 100);
      };
      checkQueue();
      return () => {};
    };

    const alice = Channel.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, undefined, 'alice', true);
    const bob = Channel.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, undefined, 'bob', false);

    const bobMessages = createMessageStream(bob);

    messageQueue.push(alice.send('Message 1'));
    const bobMessage1 = await bobMessages.next();
    expect(bobMessage1.value?.data).toBe('Message 1');

    const delayedMessage = alice.send('Message 2');

    messageQueue.push(alice.send('Message 3'));
    const bobMessage3 = await bobMessages.next();
    expect(bobMessage3.value?.data).toBe('Message 3');

    messageQueue.push(delayedMessage);

    const bobMessage2 = await bobMessages.next();
    expect(bobMessage2.value?.data).toBe('Message 2');

    expect(messageQueue.length).toBe(0);
  });
})
