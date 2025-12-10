import { describe, it, expect } from 'vitest'
import { createMockSessionManager } from './helpers/mockSessionManager'
import { MockRelay } from './helpers/mockRelay'
import { CHAT_MESSAGE_KIND } from '../src/types'

/**
 * ------------------------------------------------------------------------
 * Test cases
 * ------------------------------------------------------------------------
 */

describe('MultiDevice communication via SessionManager', () => {
  it('establishes sessions automatically and syncs messages across own devices', async () => {
    const sharedRelay = new MockRelay()

    // Create Alice's devices (same secret key)
    const { manager: alice1, secretKey: aliceSecretKey, publicKey: alicePubKey } =
      await createMockSessionManager('Alice1', sharedRelay)
    const { manager: alice2 } =
      await createMockSessionManager('Alice2', sharedRelay, aliceSecretKey)

    // Create Bob's devices (same secret key)
    const { manager: bob1, secretKey: bobSecretKey, publicKey: bobPubKey } =
      await createMockSessionManager('Bob1', sharedRelay)
    const { manager: bob2 } =
      await createMockSessionManager('Bob2', sharedRelay, bobSecretKey)

    // Track received messages per device
    const received: Record<string, any[]> = {
      alice1: [],
      alice2: [],
      bob1: [],
      bob2: [],
    }

    alice1.onEvent((e) => received.alice1.push(e))
    alice2.onEvent((e) => received.alice2.push(e))
    bob1.onEvent((e) => received.bob1.push(e))
    bob2.onEvent((e) => received.bob2.push(e))

    // Give the managers some time to publish invites and accept their peer/own invites.
    await new Promise((r) => setTimeout(r, 500))

    // Send messages - they will be queued and delivered when sessions are ready
    const alice1Promise = alice1.sendEvent(bobPubKey, { kind: CHAT_MESSAGE_KIND, content: 'Hello from Alice1' })
    const alice2Promise = alice2.sendEvent(bobPubKey, { kind: CHAT_MESSAGE_KIND, content: 'Hello from Alice2' })
    const bob1Promise = bob1.sendEvent(alicePubKey, { kind: CHAT_MESSAGE_KIND, content: 'Hello from Bob1' })

    // Wait for messages to be sent (either immediately or after queue processing)
    await Promise.all([alice1Promise, alice2Promise, bob1Promise])

    // Allow time for propagation & decryption
    await new Promise((r) => setTimeout(r, 1000))

    // --- Assertions ------------------------------------------------------

    // All devices should have received at least one chat message
    expect(received.alice1.length).toBeGreaterThan(0)
    expect(received.alice2.length).toBeGreaterThan(0)
    expect(received.bob1.length).toBeGreaterThan(0)
    expect(received.bob2.length).toBeGreaterThan(0)

    // The specific contents should be routed as expected
    const contains = (arr: any[], str: string) => arr.some((m) => m.content?.includes(str))

    // Bob devices and Alice devices (including self) received Alice1's message
    expect(contains(received.bob1, 'Alice1')).toBe(true)
    expect(contains(received.bob2, 'Alice1')).toBe(true)
    expect(contains(received.alice2, 'Alice1')).toBe(true)
    expect(contains(received.alice1, 'Alice1')).toBe(true) // self notification

    // Bob devices and Alice devices received Alice2's message
    expect(contains(received.bob1, 'Alice2')).toBe(true)
    expect(contains(received.bob2, 'Alice2')).toBe(true)
    expect(contains(received.alice1, 'Alice2')).toBe(true)
    expect(contains(received.alice2, 'Alice2')).toBe(true) // self notification

    // Alice devices and Bob2 received Bob1's message
    expect(contains(received.alice1, 'Bob1')).toBe(true)
    expect(contains(received.alice2, 'Bob1')).toBe(true)
    expect(contains(received.bob2, 'Bob1')).toBe(true)

    // Clean up
    alice1.close()
    alice2.close()
    bob1.close()
    bob2.close()
  }, 30000)
})
