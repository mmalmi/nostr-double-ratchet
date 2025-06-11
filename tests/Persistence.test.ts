import { describe, it, expect, vi } from 'vitest'
import { InMemoryStorageAdapter } from '../src/StorageAdapter'
import SessionManager from '../src/SessionManager'
import { generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools'
import { makeSubscribe, publish } from './helpers/mockRelay'
import { serializeSessionState } from '../src/utils'

/**
 * End-to-end check that sessions persisted via StorageAdapter
 * are loaded by a fresh SessionManager instance.
 */

describe('Persistence via multi-device flow', () => {
  it('restores own-device session after restart', async () => {
    // ── shared plumbing ───────────────────────────────────────────────
    const storage = new InMemoryStorageAdapter()
    const subscribe = makeSubscribe() as any

    // ── keys & devices ────────────────────────────────────────────────
    const alicePriv = generateSecretKey()
    const alicePub  = getPublicKey(alicePriv)

    // Device 2 (same user)
    const device2Id = 'alice-device-2'

    // Wrap mock relay publish with signing so that invites are signed and pass verifyEvent
    const signAndPublish = vi.fn((unsigned: any) => {
      const priv = alicePriv
      const signed = finalizeEvent(unsigned, priv)
      return publish(signed as any) as any
    })

    const aliceMgr1 = new SessionManager(alicePriv, 'alice-device-1', subscribe, signAndPublish, storage)
    await aliceMgr1.init()
    console.log('After device1 init invites in relay length:', (await storage.list()).length)

    const aliceMgrDevice2 = new SessionManager(alicePriv, device2Id, subscribe, signAndPublish, storage)
    await aliceMgrDevice2.init()

    // debug
    console.log('After device2 init keys:', await storage.list('session/'))
    console.log('Device2 userRecords:', (aliceMgrDevice2 as any).userRecords)

    // Wait for SessionManager(s) to process the invite from device1 and establish
    // a session with device2. This happens asynchronously via Invite.fromUser.
    await new Promise(r => setTimeout(r, 300))

    console.log('After wait keys:', await storage.list('session/'))

    const aliceMgr1Restarted = new SessionManager(alicePriv, 'alice-device-1',
                                                  subscribe, signAndPublish, storage)
    await aliceMgr1Restarted.init()

    const rec = (aliceMgr1Restarted as any).userRecords.get(alicePub)
    expect(rec).toBeDefined()
    // Check for the session for device2Id
    const sessions = rec.getActiveSessions();
    expect(sessions.length).toBeGreaterThan(0)
  })
}) 