import { describe, it, expect } from 'vitest'
import { Channel } from '../src/Channel'
import { getPublicKey, generateSecretKey, matchFilter } from 'nostr-tools'
import { EVENT_KIND, KeyType, Sender } from '../src/types';
import { createMessageStream } from '../src/utils';

describe('Channel', () => {
  const aliceSecretKey = generateSecretKey()
  const bobSecretKey = generateSecretKey()
  const dummyUnsubscribe = () => {}
  const dummySubscribe = () => dummyUnsubscribe

  it('should initialize with correct properties', () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey)

    expect(alice.state.theirCurrentNostrPublicKey).toBe(getPublicKey(bobSecretKey))
    expect(alice.state.ourCurrentNostrKey.publicKey).toBe(getPublicKey(aliceSecretKey))
    expect(alice.state.ourCurrentNostrKey.publicKey).toHaveLength(64) // Hex-encoded public key length
  })

  it('should create an event with correct properties', () => {
    const channel = Channel.init(() => dummyUnsubscribe, getPublicKey(bobSecretKey), aliceSecretKey)
    const testData = 'Hello, world!'

    const event = channel.send(testData)

    expect(event).toBeTruthy()
    expect(event.kind).toBe(EVENT_KIND)
    expect(event.tags).toEqual([])
    expect(event.content).toBeTruthy()
    expect(typeof event.created_at).toBe('number')
    expect(event.pubkey).toHaveLength(64)
    expect(event.id).toHaveLength(64)
    expect(event.sig).toHaveLength(128)
  })

  it('should create channels with correct receiving and sending chain keys', () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey, 'alice')
    const bob = Channel.init(dummySubscribe, getPublicKey(aliceSecretKey), bobSecretKey, 'bob')
    expect(alice.state.receivingChainKey).toEqual(bob.state.sendingChainKey)
    expect(alice.state.sendingChainKey).toEqual(bob.state.receivingChainKey)
    expect(alice.getNostrSenderKeypair(Sender.Us, KeyType.Current)).toEqual(bob.getNostrSenderKeypair(Sender.Them, KeyType.Current))
  })

  it('should handle incoming events and update keys', async () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey, 'alice')
    const event = alice.send('Hello, Bob!')
    
    const bob = Channel.init((filter, onEvent) => {
      console.log('filter.authors', filter.authors, 'event.pubkey', event.pubkey)
      if (matchFilter(filter, event)) {
        onEvent(event)
      }
      return dummyUnsubscribe
    }, getPublicKey(aliceSecretKey), bobSecretKey, 'bob')

    expect(event.pubkey).toBe(bob.getNostrSenderKeypair(Sender.Them, KeyType.Current).publicKey)

    const aliceInitialNextPublicKey = alice.state.ourNextNostrKey.publicKey
    const bobInitialNextPublicKey = bob.state.ourNextNostrKey.publicKey

    const bobMessages = createMessageStream(bob);

    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.data).toBe('Hello, Bob!')
    expect(bob.state.theirCurrentNostrPublicKey).toBe(aliceInitialNextPublicKey)
    expect(bob.state.ourNextNostrKey.publicKey).toBe(bobInitialNextPublicKey)
    expect(bob.state.ourCurrentNostrKey.publicKey).toBe(getPublicKey(bobSecretKey))
    expect(alice.state.ourCurrentNostrKey.publicKey).toBe(getPublicKey(aliceSecretKey))
    expect(alice.getNostrSenderKeypair(Sender.Them, KeyType.Next)).toEqual(bob.getNostrSenderKeypair(Sender.Us, KeyType.Current))
  })

  it('should handle multiple back-and-forth messages correctly', async () => {
    const messageQueue: any[] = [];

    const createSubscribe = (name: string) => (filter: any, onEvent: (event: any) => void) => {
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

    const alice = Channel.init(createSubscribe('Alice'), getPublicKey(bobSecretKey), aliceSecretKey, 'alice');
    const bob = Channel.init(createSubscribe('Bob'), getPublicKey(aliceSecretKey), bobSecretKey, 'bob');

    const aliceMessages = createMessageStream(alice);
    const bobMessages = createMessageStream(bob);

    const sendAndExpect = async (sender: Channel, receiver: AsyncIterableIterator<any>, message: string) => {
      messageQueue.push(sender.send(message));
      const receivedMessage = await receiver.next();
      expect(receivedMessage.value?.data).toBe(message);
    };

    // Test conversation
    await sendAndExpect(alice, bobMessages, 'Hello Bob!');
    await sendAndExpect(bob, aliceMessages, 'Hi Alice!');
    await sendAndExpect(alice, bobMessages, 'How are you?');

    // Test consecutive messages from Bob
    await sendAndExpect(bob, aliceMessages, 'I am fine, thank you!');
    await sendAndExpect(bob, aliceMessages, 'How about you?');

    // Final message from Alice
    await sendAndExpect(alice, bobMessages, "I'm doing great, thanks!");

    // No remaining messagess
    expect(messageQueue.length).toBe(0);
  })
})