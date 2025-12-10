import { describe, it, expect } from 'vitest'
import { createMockSessionManager } from './helpers/mockSessionManager'
import { MockRelay } from './helpers/mockRelay'

/**
 * End-to-end check that sessions persisted via StorageAdapter
 * are loaded by a fresh SessionManager instance.
 */

describe('Persistence via multi-device flow', () => {
  it('restores own-device session after restart', async () => {
    const sharedRelay = new MockRelay()

    // Create first device for Alice
    const {
      manager: aliceMgr1,
      secretKey: aliceSecretKey,
      publicKey: alicePubKey,
      mockStorage: storage,
    } = await createMockSessionManager('alice-device-1', sharedRelay)

    console.log('After device1 init invites in relay length:', (await storage.list()).length)

    // Create second device for Alice (same secret key, same storage)
    const { manager: aliceMgrDevice2 } = await createMockSessionManager(
      'alice-device-2',
      sharedRelay,
      aliceSecretKey,
      storage
    )

    // debug
    console.log('After device2 init keys:', await storage.list('v1/session/'))
    console.log('Device2 userRecords:', (aliceMgrDevice2 as any).userRecords)

    // Wait for SessionManager(s) to process the invite from device1 and establish
    // a session with device2. This happens asynchronously via Invite.fromUser.
    await new Promise(r => setTimeout(r, 300))

    console.log('After wait keys:', await storage.list('v1/session/'))

    // Close the first manager and restart it
    aliceMgr1.close()

    const { manager: aliceMgr1Restarted } = await createMockSessionManager(
      'alice-device-1',
      sharedRelay,
      aliceSecretKey,
      storage
    )

    const rec = (aliceMgr1Restarted as any).userRecords.get(alicePubKey)
    expect(rec).toBeDefined()
    // Check for devices
    const devices = Array.from(rec?.devices?.values() || [])
    expect(devices.length).toBeGreaterThan(0)
  })
})
