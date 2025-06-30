import { describe, it, expect } from 'vitest'
import SessionManager from '../src/SessionManager'
import { generateSecretKey, getPublicKey, matchFilter } from 'nostr-tools'

/**
 * Utilities --------------------------------------------------------------
 */

// Shared in-memory "network" for all simulated devices in this test run.
const messageQueue: any[] = []

/**
 * Create a Nostr subscribe stub for a simulated device. It polls the shared
 * messageQueue and emits any events that match the provided filter.
 */
function createSubscribe(name: string) {
  const processedEventIds = new Set<string>()
  
  return (filter: any, onEvent: (event: any) => void) => {
    const tick = () => {
      for (const ev of messageQueue) {
        if (matchFilter(filter, ev) && !processedEventIds.has(ev.id)) {
          processedEventIds.add(ev.id)
          onEvent(ev)
        }
      }
      setTimeout(tick, 10) // keep polling
    }
    tick()
    // Unsubscribe stub (no-op for the polling implementation)
    return () => {}
  }
}

/**
 * Factory function to create a nostrPublish function with access to the keys
 */
function createNostrPublish(aliceKey: Uint8Array, bobKey: Uint8Array, alicePubKey: string, bobPubKey: string) {
  return async function nostrPublish(event: any) {
    const { finalizeEvent } = await import('nostr-tools')
    
    if (event.kind === 30078) {
      let privateKey: Uint8Array
      if (event.pubkey === alicePubKey) {
        privateKey = aliceKey
      } else if (event.pubkey === bobPubKey) {
        privateKey = bobKey
      } else {
        privateKey = aliceKey
      }
      const signedEvent = finalizeEvent(event, privateKey)
      messageQueue.push(signedEvent)
      return signedEvent
    }
    
    if (event.sig) {
      messageQueue.push(event)
      return event
    }
    
    let privateKey: Uint8Array
    if (event.pubkey === alicePubKey) {
      privateKey = aliceKey
    } else if (event.pubkey === bobPubKey) {
      privateKey = bobKey
    } else {
      privateKey = aliceKey // fallback
    }
    
    const signedEvent = finalizeEvent(event, privateKey)
    messageQueue.push(signedEvent)
    return signedEvent
  }
}

/**
 * ------------------------------------------------------------------------
 * Test cases
 * ------------------------------------------------------------------------
 */

describe('MultiDevice communication via SessionManager', () => {
  it('establishes sessions automatically and syncs messages across own devices', async () => {
    // Generate identities
    const aliceKey = generateSecretKey()
    const bobKey = generateSecretKey()
    const alicePubKey = getPublicKey(aliceKey)
    const bobPubKey = getPublicKey(bobKey)

    // Create nostrPublish function with access to the keys
    const nostrPublish = createNostrPublish(aliceKey, bobKey, alicePubKey, bobPubKey)

    // Create one SessionManager per simulated device
    const alice1 = new SessionManager(aliceKey, 'Alice1', createSubscribe('Alice1'), nostrPublish)
    const alice2 = new SessionManager(aliceKey, 'Alice2', createSubscribe('Alice2'), nostrPublish)
    const bob1 = new SessionManager(bobKey, 'Bob1', createSubscribe('Bob1'), nostrPublish)
    const bob2 = new SessionManager(bobKey, 'Bob2', createSubscribe('Bob2'), nostrPublish)

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

    // Wait for SessionManager initialization to complete
    await alice1.init()
    await alice2.init()
    await bob1.init()
    await bob2.init()


    // Give the managers some time to publish invites and accept their peer/own invites.
    await new Promise((r) => setTimeout(r, 2000))

    // Helper to keep trying to send until sessions are ready
    async function sendWhenReady(manager: SessionManager, recipient: string, content: string) {
      let attempts = 0
      while (attempts < 20) {
        const evs = await manager.sendText(recipient, content)
        if (evs.length > 0) {
          evs.forEach((ev) => messageQueue.push(ev))
          return
        }
        attempts++
        await new Promise((r) => setTimeout(r, 500))
      }
      throw new Error('Unable to establish session to send message')
    }

    // Alice1 sends a message to Bob (should reach Bob1, Bob2 and Alice2)
    await sendWhenReady(alice1, bobPubKey, 'Hello from Alice1')

    // Bob1 replies (should reach Alice1, Alice2 and Bob2)
    await sendWhenReady(bob1, alicePubKey, 'Hello from Bob1')

    // Allow time for propagation & decryption
    await new Promise((r) => setTimeout(r, 3000))

    // --- Assertions ------------------------------------------------------

    // All devices should have received at least one chat message
    expect(received.alice1.length).toBeGreaterThan(0)
    expect(received.alice2.length).toBeGreaterThan(0)
    expect(received.bob1.length).toBeGreaterThan(0)
    expect(received.bob2.length).toBeGreaterThan(0)

    // The specific contents should be routed as expected
    const contains = (arr: any[], str: string) => arr.some((m) => m.content?.includes(str))

    // Bob devices and Alice2 received Alice1's message
    expect(contains(received.bob1, 'Alice1')).toBe(true)
    expect(contains(received.bob2, 'Alice1')).toBe(true)
    expect(contains(received.alice2, 'Alice1')).toBe(true)

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
