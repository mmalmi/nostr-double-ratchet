import { describe, it, expect, beforeAll } from 'vitest'
import { Channel } from '../src/Channel'
import { getPublicKey, generateSecretKey, VerifiedEvent, Filter } from 'nostr-tools'
import { createMessageStream } from '../src/utils';
import NDK, { NDKEvent } from '@nostr-dev-kit/ndk'

describe('Channel', () => {
	let ndk: NDK;
	const aliceSecretKey = generateSecretKey();
	const bobSecretKey = generateSecretKey();

	beforeAll(async () => {
		ndk = new NDK({ explicitRelayUrls: ['wss://strfry.iris.to'] })
		
		console.log('Attempting to connect to NDK...')
		try {
			await ndk.connect()
			console.log('NDK connected successfully')
		} catch (error) {
			console.error('Failed to connect to NDK:', error)
			throw error; // This will cause the tests to fail if connection fails
		}

		await new Promise((resolve) => {
			setTimeout(() => {
				const relays = Array.from(ndk.pool.relays.entries())
				console.log('Relays:', relays.map(([url, relay]) => ({ url, status: relay.status })))
				resolve(null)
			}, 3000)
		})
	})

	const subscribe = (filter: Filter, onEvent: (event: VerifiedEvent) => void) => {
		const sub = ndk.subscribe(filter)
		sub.on("event", (event) => {
			console.log(event)
			onEvent(event)
		})
		return () => {} // no need to sub.stop(), old nostr senders might still have unseen?
	}

	const publish = async (event: VerifiedEvent) => {
		const evt = new NDKEvent(ndk, event)
		expect(evt.verifySignature(false)).toBe(true)
		await evt.publish()
	}

	it('should handle multiple back-and-forth messages correctly', async () => {
		const relay = ndk.pool.relays.entries().next().value[1]
		console.log('Relay connection details:', {
			url: relay.url,
			status: relay.status,
			connectionState: relay.connectionState,
		})
		
		expect(relay.status).toBe(1) // connected

		console.log('Test started: multiple back-and-forth messages');

		// Initialize Alice's and Bob's channels
		const alice = Channel.init(subscribe, getPublicKey(bobSecretKey), aliceSecretKey, true, new Uint8Array(), 'alice');
		const bob = Channel.init(subscribe, getPublicKey(aliceSecretKey), bobSecretKey, false, new Uint8Array(), 'bob');

		const aliceMessages = createMessageStream(alice);
		const bobMessages = createMessageStream(bob);

		// Alice sends first message
		await publish(alice.send('Hello Bob!'));
		console.log('Alice sent: Hello Bob!');

		// Bob receives and sends a reply
		const bobFirstMessage = await bobMessages.next();
		expect(bobFirstMessage.value?.data).toBe('Hello Bob!');
		console.log('Bob received: Hello Bob!');

		await publish(bob.send('Hi Alice!'));
		console.log('Bob sent: Hi Alice!');

		// Alice receives Bob's message and replies
		const aliceSecondMessage = await aliceMessages.next();
		expect(aliceSecondMessage.value?.data).toBe('Hi Alice!');
		console.log('Alice received: Hi Alice!');

		await publish(alice.send('How are you?'));
		console.log('Alice sent: How are you?');

		// Bob receives Alice's second message and replies
		const bobSecondMessage = await bobMessages.next();
		expect(bobSecondMessage.value?.data).toBe('How are you?');
		console.log('Bob received: How are you?');

		await publish(bob.send('I am fine, thank you!'));
		console.log('Bob sent: I am fine, thank you!');

		// Bob sends a second consecutive message
		await publish(bob.send('How about you?'));
		console.log('Bob sent: How about you?');

		// Check if Alice receives Bob's last two messages
		const aliceThirdMessage = await aliceMessages.next();
		expect(aliceThirdMessage.value?.data).toBe('I am fine, thank you!');
		console.log('Alice received: I am fine, thank you!');

		const aliceFourthMessage = await aliceMessages.next();
		expect(aliceFourthMessage.value?.data).toBe('How about you?');
		console.log('Alice received: How about you?');

		console.log('Test completed successfully');
	})
})
