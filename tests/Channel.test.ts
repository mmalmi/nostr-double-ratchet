import { describe, it, expect } from 'vitest'
import { Channel } from '../src/Channel'
import { getPublicKey, generateSecretKey, matchFilter } from 'nostr-tools'
import { MESSAGE_EVENT_KIND } from '../src/types';
import { createMessageStream } from '../src/utils';
import { serializeChannelState, deserializeChannelState } from '../src/utils';

describe('Channel', () => {
  const aliceSecretKey = generateSecretKey()
  const bobSecretKey = generateSecretKey()
  const dummyUnsubscribe = () => {}
  const dummySubscribe = () => dummyUnsubscribe

  it('should initialize with correct properties', () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array())

    expect(alice.state.theirNostrPublicKey).toBe(getPublicKey(bobSecretKey))
    expect(alice.state.ourCurrentNostrKey!.publicKey).toBe(getPublicKey(aliceSecretKey))
    expect(alice.state.ourCurrentNostrKey!.publicKey).toHaveLength(64) // Hex-encoded public key length
  })

  it('should create an event with correct properties', () => {
    const channel = Channel.init(() => dummyUnsubscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice')
    const testData = 'Hello, world!'

    const event = channel.send(testData)

    expect(event).toBeTruthy()
    expect(event.kind).toBe(MESSAGE_EVENT_KIND)
    expect(event.tags[0][0]).toEqual("header")
    expect(event.tags[0][1]).toBeTruthy()
    expect(event.content).toBeTruthy()
    expect(typeof event.created_at).toBe('number')
    expect(event.pubkey).toHaveLength(64)
    expect(event.id).toHaveLength(64)
    expect(event.sig).toHaveLength(128)
  })

  it('should handle incoming events and update keys', async () => {
    const alice = Channel.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice')
    const event = alice.send('Hello, Bob!')
    
    const bob = Channel.init((filter, onEvent) => {
      if (matchFilter(filter, event)) {
        onEvent(event)
      }
      return dummyUnsubscribe
    }, getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob')

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

    const alice = Channel.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Channel.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createMessageStream(alice);
    const bobMessages = createMessageStream(bob);

    const sendAndExpect = async (sender: Channel, receiver: AsyncIterableIterator<any>, message: string, receiverChannel: Channel) => {
      const initialSendingChainKey = sender.state.sendingChainKey;
      const initialReceivingChainKey = receiverChannel.state.receivingChainKey;
      const initialOurCurrentNostrKey = receiverChannel.state.ourCurrentNostrKey?.publicKey;
      const initialTheirNostrPublicKey = receiverChannel.state.theirNostrPublicKey;

      messageQueue.push(sender.send(message));
      const receivedMessage = await receiver.next();

      console.log(`${receiverChannel.name} got from ${sender.name}: ${receivedMessage.value.data}`)
      expect(receivedMessage.value?.data).toBe(message);

      // Check that the chain keys have changed
      expect(sender.state.sendingChainKey).not.toBe(initialSendingChainKey);
      expect(receiverChannel.state.receivingChainKey).not.toBe(initialReceivingChainKey);

      // Check that the keys change when the first message of consecutive messages is received
      if (receiverChannel.state.receivingChainMessageNumber === 1) {
        expect(receiverChannel.state.ourCurrentNostrKey?.publicKey).not.toBe(initialOurCurrentNostrKey);
        expect(receiverChannel.state.theirNostrPublicKey).not.toBe(initialTheirNostrPublicKey);
      }
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

    // No remaining messages
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

    const alice = Channel.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Channel.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

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

  it('should maintain conversation state through serialization', async () => {
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

    // Initialize channels
    const alice = Channel.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Channel.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createMessageStream(alice);
    const bobMessages = createMessageStream(bob);

    // Send initial messages
    messageQueue.push(alice.send('Hello Bob!'));
    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.data).toBe('Hello Bob!');

    messageQueue.push(bob.send('Hi Alice!'));
    const aliceFirstMessage = await aliceMessages.next();
    expect(aliceFirstMessage.value?.data).toBe('Hi Alice!');

    // Serialize both channel states
    const serializedAlice = serializeChannelState(alice.state);
    const serializedBob = serializeChannelState(bob.state);

    // Create new channels with deserialized state
    const aliceRestored = new Channel(createSubscribe(), deserializeChannelState(serializedAlice));
    const bobRestored = new Channel(createSubscribe(), deserializeChannelState(serializedBob));

    const aliceRestoredMessages = createMessageStream(aliceRestored);
    const bobRestoredMessages = createMessageStream(bobRestored);

    // Continue conversation with restored channels
    messageQueue.push(aliceRestored.send('How are you?'));
    const bobSecondMessage = await bobRestoredMessages.next();
    expect(bobSecondMessage.value?.data).toBe('How are you?');

    messageQueue.push(bobRestored.send('Doing great!'));
    const aliceSecondMessage = await aliceRestoredMessages.next();
    expect(aliceSecondMessage.value?.data).toBe('Doing great!');

    expect(messageQueue.length).toBe(0);
  });

  it('should subscribe to public keys from skipped messages', async () => {
    const messageQueue: any[] = [];

    function createSubscribe() {
      let unsubscribed = false;
    
      return (filter: any, onEvent: (event: any) => void) => {
        function checkQueue() {
          if (unsubscribed) return;
          const index = messageQueue.findIndex(event => matchFilter(filter, event));
          if (index !== -1) {
            onEvent(messageQueue.splice(index, 1)[0]);
          }
          setTimeout(checkQueue, 100);
        }
        checkQueue();
    
        return () => {
          unsubscribed = true;
        };
      };
    }

    // Initialize channels
    const alice = Channel.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Channel.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createMessageStream(alice);
    const bobMessages = createMessageStream(bob);

    // Create some skipped messages by sending out of order
    const message1 = alice.send('Message 1');
    const message2 = alice.send('Message 2');
    const message3 = alice.send('Message 3');

    // Deliver messages out of order to create skipped messages
    messageQueue.push(message3);
    await bobMessages.next();

    // Verify skipped keys structure
    expect(bob.state.skippedKeys[getPublicKey(aliceSecretKey)]).toBeDefined();
    expect(Object.keys(bob.state.skippedKeys[getPublicKey(aliceSecretKey)].messageKeys)).toHaveLength(2);
    expect(bob.state.skippedKeys[getPublicKey(aliceSecretKey)].headerKeys).toHaveLength(1);

    const message4 = bob.send('Message 4');
    messageQueue.push(message4);
    await aliceMessages.next();

    const message5 = alice.send('Acknowledge message 4');
    messageQueue.push(message5);
    await bobMessages.next();

    // Serialize bob's state and create a new channel
    const serializedBob = serializeChannelState(bob.state);

    // Prevent old channel from capturing from the test message queue
    bob.close()
   
    // Old messages are delivered late
    messageQueue.push(message1);
    messageQueue.push(message2);
    
    // Create new channel with serialized state
    const bobRestored = new Channel(createSubscribe(), deserializeChannelState(serializedBob));
    bobRestored.name = 'bobRestored';
    const bobMessages2 = createMessageStream(bobRestored);

    // Verify skipped keys were properly serialized and deserialized
    expect(bobRestored.state.skippedKeys[getPublicKey(aliceSecretKey)]).toBeDefined();
    expect(Object.keys(bobRestored.state.skippedKeys[getPublicKey(aliceSecretKey)].messageKeys)).toHaveLength(2);
    expect(bobRestored.state.skippedKeys[getPublicKey(aliceSecretKey)].headerKeys).toHaveLength(1);

    const skippedMessage1 = await bobMessages2.next();
    expect(skippedMessage1.value?.data).toBe('Message 1');
    const skippedMessage2 = await bobMessages2.next();
    expect(skippedMessage2.value?.data).toBe('Message 2');

    // Verify skipped keys were cleaned up after processing
    console.log(bobRestored.state.skippedKeys)
    expect(Object.keys(bobRestored.state.skippedKeys)).toHaveLength(0);
  });
})
