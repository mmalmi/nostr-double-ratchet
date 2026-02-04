import { describe, it, expect } from 'vitest'
import { Session } from '../src/Session'
import { getPublicKey, generateSecretKey, matchFilter } from 'nostr-tools'
import { MESSAGE_EVENT_KIND, REACTION_KIND, CHAT_MESSAGE_KIND } from '../src/types';
import { createEventStream, parseReaction, isReaction } from '../src/utils';
import { serializeSessionState, deserializeSessionState } from '../src/utils';

describe('Session', () => {
  const aliceSecretKey = generateSecretKey()
  const bobSecretKey = generateSecretKey()
  const dummyUnsubscribe = () => {}
  const dummySubscribe = () => dummyUnsubscribe

  it('should initialize with correct properties', () => {
    const alice = Session.init(dummySubscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array())

    expect(alice.state.theirNextNostrPublicKey).toBe(getPublicKey(bobSecretKey))
    expect(alice.state.ourCurrentNostrKey!.publicKey).toBe(getPublicKey(aliceSecretKey))
    expect(alice.state.ourCurrentNostrKey!.publicKey).toHaveLength(64) // Hex-encoded public key length
  })

  it('should create an event with correct properties', () => {
    const session = Session.init(() => dummyUnsubscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice')
    const testData = 'Hello, world!'

    const {event} = session.send(testData)

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
    // Create a message queue to simulate network events
    const messageQueue: any[] = [];

    const subscribe = (filter: any, onEvent: (event: any) => void) => {
      const checkQueue = () => {
        const index = messageQueue.findIndex(event => matchFilter(filter, event));
        if (index !== -1) {
          onEvent(messageQueue.splice(index, 1)[0]);
        }
      };
      // Immediate check for test purposes
      checkQueue();
      return dummyUnsubscribe;
    };

    const alice = Session.init(subscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(subscribe, getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const initialReceivingChainKey = bob.state.receivingChainKey;
    const bobMessages = createEventStream(bob);

    // Push the message to the queue
    messageQueue.push(alice.send('Hello, Bob!').event);

    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.content).toBe('Hello, Bob!');
    expect(bob.state.receivingChainKey).not.toBe(initialReceivingChainKey);
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

    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    const bobMessages = createEventStream(bob);

    const sendAndExpect = async (sender: Session, receiver: AsyncIterableIterator<any>, message: string, receiverSession: Session) => {
      const initialSendingChainKey = sender.state.sendingChainKey;
      const initialReceivingChainKey = receiverSession.state.receivingChainKey;
      const initialOurCurrentNostrKey = receiverSession.state.ourCurrentNostrKey?.publicKey;
      const initialTheirNostrPublicKey = receiverSession.state.theirNextNostrPublicKey;

      messageQueue.push(sender.send(message).event);
      const receivedMessage = await receiver.next();

      console.log(`${receiverSession.name} got from ${sender.name}: ${receivedMessage.value.content}`)
      expect(receivedMessage.value?.content).toBe(message);

      // Check that the chain keys have changed
      expect(sender.state.sendingChainKey).not.toBe(initialSendingChainKey);
      expect(receiverSession.state.receivingChainKey).not.toBe(initialReceivingChainKey);

      // Check that the keys change when the first message of consecutive messages is received
      if (receiverSession.state.receivingChainMessageNumber === 1) {
        expect(receiverSession.state.ourCurrentNostrKey?.publicKey).not.toBe(initialOurCurrentNostrKey);
        expect(receiverSession.state.theirNextNostrPublicKey).not.toBe(initialTheirNostrPublicKey);
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

    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const bobMessages = createEventStream(bob);

    messageQueue.push(alice.send('Message 1').event);
    const bobMessage1 = await bobMessages.next();
    expect(bobMessage1.value?.content).toBe('Message 1');

    const delayedMessage = alice.send('Message 2').event;

    messageQueue.push(alice.send('Message 3').event);
    const bobMessage3 = await bobMessages.next();
    expect(bobMessage3.value?.content).toBe('Message 3');

    messageQueue.push(delayedMessage);

    const bobMessage2 = await bobMessages.next();
    expect(bobMessage2.value?.content).toBe('Message 2');

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

    // Initialize sessions
    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    const bobMessages = createEventStream(bob);

    // Send initial messages
    messageQueue.push(alice.send('Hello Bob!').event);
    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.content).toBe('Hello Bob!');

    messageQueue.push(bob.send('Hi Alice!').event);
    const aliceFirstMessage = await aliceMessages.next();
    expect(aliceFirstMessage.value?.content).toBe('Hi Alice!');

    // Serialize both session states
    const serializedAlice = serializeSessionState(alice.state);
    const serializedBob = serializeSessionState(bob.state);

    // Create new sessions with deserialized state
    const aliceRestored = new Session(createSubscribe(), deserializeSessionState(serializedAlice));
    const bobRestored = new Session(createSubscribe(), deserializeSessionState(serializedBob));

    const aliceRestoredMessages = createEventStream(aliceRestored);
    const bobRestoredMessages = createEventStream(bobRestored);

    // Continue conversation with restored sessions
    messageQueue.push(aliceRestored.send('How are you?').event);
    const bobSecondMessage = await bobRestoredMessages.next();
    expect(bobSecondMessage.value?.content).toBe('How are you?');

    messageQueue.push(bobRestored.send('Doing great!').event);
    const aliceSecondMessage = await aliceRestoredMessages.next();
    expect(aliceSecondMessage.value?.content).toBe('Doing great!');

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

    // Initialize sessions
    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    const bobMessages = createEventStream(bob);

    // Create some skipped messages by sending out of order
    const message1 = alice.send('Message 1').event;
    const message2 = alice.send('Message 2').event;
    const message3 = alice.send('Message 3').event;

    // Deliver messages out of order to create skipped messages
    messageQueue.push(message3);
    await bobMessages.next();

    // At this point, message1 and message2 are skipped and stored in skippedMessageKeys

    const message4 = bob.send('Message 4').event;
    messageQueue.push(message4);
    await aliceMessages.next();

    const message5 = alice.send('Acknowledge message 4').event;
    messageQueue.push(message5);
    await bobMessages.next();
    // Bob now has next key from Alice

    // Serialize bob's state and create a new session
    const serializedBob = serializeSessionState(bob.state);

    // Prevent old session from capturing from the test message queue
    bob.close()
   
    // Old messages are delivered late
    messageQueue.push(message1);
    messageQueue.push(message2);
    
    // Create new session with serialized state
    const bobRestored = new Session(createSubscribe(), deserializeSessionState(serializedBob));
    bobRestored.name = 'bobRestored';
    const bobMessages2 = createEventStream(bobRestored); // This triggers subscriptions
    // Deliver the skipped message

    const skippedMessage1 = await bobMessages2.next();
    expect(skippedMessage1.value?.content).toBe('Message 1');
    const skippedMessage2 = await bobMessages2.next();
    expect(skippedMessage2.value?.content).toBe('Message 2');
  });

  it('should discard duplicate messages after restoring', async () => {
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

    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    const bobMessages = createEventStream(bob);

    const sentEvents: any[] = [];
    const messages = ['Message 1', 'Message 2', 'Message 3'];

    for (const message of messages) {
      const { event } = alice.send(message);
      sentEvents.push(event);
      messageQueue.push(event);
      const received = await bobMessages.next();
      expect(received.value?.content).toBe(message);
    }

    const serializedBob = serializeSessionState(bob.state);
    bob.close();

    const bobRestored = new Session(createSubscribe(), deserializeSessionState(serializedBob));
    const initialReceivingCount = bobRestored.state.receivingChainMessageNumber;

    for (const event of sentEvents) {
      messageQueue.push(event);
    }

    // Give the restored session time to process and discard duplicate ciphertexts
    await new Promise(resolve => setTimeout(resolve, 300));
    expect(bobRestored.state.receivingChainMessageNumber).toBe(initialReceivingCount);

    const { event: bobReply } = bobRestored.send('Fresh message after duplicates');
    messageQueue.push(bobReply);

    const aliceReceived = await aliceMessages.next();
    expect(aliceReceived.value?.content).toBe('Fresh message after duplicates');
  });

  it('should handle session reinitialization correctly', async () => {
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

    // Initialize sessions
    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    let bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    let bobMessages = createEventStream(bob);

    // Alice sends first message
    messageQueue.push(alice.send('Message 1').event);
    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.content).toBe('Message 1');

    // Bob closes his session and reinitializes with serialized state
    const serializedBobState = serializeSessionState(bob.state);
    bob.close();

    bob = new Session(createSubscribe(), deserializeSessionState(serializedBobState));
    bobMessages = createEventStream(bob);

    // Alice sends second message
    messageQueue.push(alice.send('Message 2').event);
    const bobSecondMessage = await bobMessages.next();
    expect(bobSecondMessage.value?.content).toBe('Message 2');

    expect(messageQueue.length).toBe(0);
  });

  it('should send and receive reactions correctly', async () => {
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

    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    const bobMessages = createEventStream(bob);

    // Alice sends a message
    const { event: messageEvent, innerEvent: messageInner } = alice.send('Hello Bob!');
    messageQueue.push(messageEvent);
    const bobFirstMessage = await bobMessages.next();
    expect(bobFirstMessage.value?.content).toBe('Hello Bob!');

    // Bob sends a reaction to Alice's message
    const messageId = messageInner.id;
    const { event: reactionEvent, innerEvent: reactionInner } = bob.sendReaction(messageId, '👍');
    
    // Verify reaction event structure
    expect(reactionInner.kind).toBe(REACTION_KIND);
    expect(reactionInner.tags).toContainEqual(['e', messageId]);
    
    // Verify reaction content is raw emoji (NIP-25 compatible)
    expect(reactionInner.content).toBe('👍');
    expect(isReaction(reactionInner)).toBe(true);
    const payload = parseReaction(reactionInner);
    expect(payload).not.toBeNull();
    expect(payload?.emoji).toBe('👍');
    expect(payload?.messageId).toBe(messageId);

    // Alice receives the reaction
    messageQueue.push(reactionEvent);
    const aliceReaction = await aliceMessages.next();
    expect(isReaction(aliceReaction.value!)).toBe(true);
    const receivedPayload = parseReaction(aliceReaction.value!);
    expect(receivedPayload?.emoji).toBe('👍');
    expect(receivedPayload?.messageId).toBe(messageId);
  });

  it('should correctly identify reaction vs regular messages', () => {
    // Test reaction rumor
    const reactionRumor = { kind: REACTION_KIND, content: '❤️', tags: [["e", "abc123"]] };
    expect(isReaction(reactionRumor)).toBe(true);
    const parsed = parseReaction(reactionRumor);
    expect(parsed?.type).toBe('reaction');
    expect(parsed?.messageId).toBe('abc123');
    expect(parsed?.emoji).toBe('❤️');

    // Test regular message rumor
    const messageRumor = { kind: 14, content: 'Hello world', tags: [] };
    expect(isReaction(messageRumor)).toBe(false);
    expect(parseReaction(messageRumor)).toBeNull();

    // Test rumor with wrong kind
    const wrongKind = { kind: 1, content: '👍', tags: [["e", "abc123"]] };
    expect(isReaction(wrongKind)).toBe(false);
    expect(parseReaction(wrongKind)).toBeNull();
  });

  it('should send and receive replies correctly', async () => {
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

    const alice = Session.init(createSubscribe(), getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
    const bob = Session.init(createSubscribe(), getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

    const aliceMessages = createEventStream(alice);
    const bobMessages = createEventStream(bob);

    // Alice sends a message
    const { event: messageEvent, innerEvent: messageInner } = alice.send('Hello Bob!');
    messageQueue.push(messageEvent);
    await bobMessages.next();

    // Bob replies to Alice's message
    const messageId = messageInner.id;
    const { event: replyEvent, innerEvent: replyInner } = bob.sendReply('Hey Alice, great to hear from you!', messageId);

    // Verify reply is a chat message (not a reaction)
    expect(replyInner.kind).toBe(CHAT_MESSAGE_KIND);
    expect(replyInner.content).toBe('Hey Alice, great to hear from you!');
    expect(replyInner.tags).toContainEqual(['e', messageId]);

    // Alice receives the reply
    messageQueue.push(replyEvent);
    const aliceReply = await aliceMessages.next();
    expect(aliceReply.value?.content).toBe('Hey Alice, great to hear from you!');
    expect(aliceReply.value?.kind).toBe(CHAT_MESSAGE_KIND);
    expect(aliceReply.value?.tags).toContainEqual(['e', messageId]);
  });
})
