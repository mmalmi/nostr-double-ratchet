import { describe, it, expect, vi } from 'vitest'
import SessionManager from '../src/SessionManager'
import { generateSecretKey } from 'nostr-tools'
import { CHAT_MESSAGE_KIND } from '../src/types'
import { UserRecord } from '../src/UserRecord'
import type { Session } from '../src/Session'

/**
 * Helper to create a lightweight stub that satisfies the parts of the Session
 * interface that SessionManager relies on (sendEvent, onEvent, close).
 */
function createStubSession() {
  const callbacks: ((event: any) => void)[] = []
  const stub: any = {
    name: 'stub',
    sendEvent: vi.fn().mockImplementation((event: any) => {
      // Simulate returning an encrypted event wrapper
      return { event: { ...event, id: 'id-' + Math.random().toString(36).slice(2) } }
    }),
    onEvent: vi.fn().mockImplementation((cb: (event: any) => void) => {
      callbacks.push(cb)
      return () => {}
    }),
    close: vi.fn(),
    // Helper to emit an incoming event for tests
    _emit: (event: any) => {
      callbacks.forEach((cb) => cb(event))
    },
  }
  return stub as unknown as Session & { _emit: (event: any) => void }
}

describe('SessionManager', () => {
  const nostrSubscribe = vi.fn().mockReturnValue(() => {})
  const nostrPublish = vi.fn()
  const ourIdentityKey = generateSecretKey()

  it('should start listening and throw when no active session exists', async () => {
    const manager = new SessionManager(ourIdentityKey, nostrSubscribe, nostrPublish)
    const listenSpy = vi.spyOn(manager as any, 'listenToUser')

    await expect(manager.sendText('recipient', 'hello')).rejects.toThrow('No active session with user')
    expect(listenSpy).toHaveBeenCalledWith('recipient')
  })

  it('should send events to all active sessions', async () => {
    const manager = new SessionManager(ourIdentityKey, nostrSubscribe, nostrPublish)

    const recipient = 'recipientPubKey'
    const session = createStubSession()
    const userRecord = new UserRecord(recipient, nostrSubscribe)
    userRecord.addSession(session)
    ;(manager as any).userRecords.set(recipient, userRecord)

    const results = await manager.sendText(recipient, 'hello')

    expect(session.sendEvent).toHaveBeenCalledTimes(1)
    expect(session.sendEvent).toHaveBeenCalledWith({ kind: CHAT_MESSAGE_KIND, content: 'hello' })
    expect(results).toHaveLength(1)
  })

  it('should propagate incoming session events to listeners', () => {
    const manager = new SessionManager(ourIdentityKey, nostrSubscribe, nostrPublish)

    const recipient = 'recipientPubKey'
    const session = createStubSession()
    const userRecord = new UserRecord(recipient, nostrSubscribe)
    userRecord.addSession(session)
    ;(manager as any).userRecords.set(recipient, userRecord)

    const received: any[] = []
    manager.onEvent((e) => received.push(e))

    const testEvent = { content: 'incoming' }
    ;(session as any)._emit(testEvent)
    expect(received).toHaveLength(1)
    expect(received[0]).toBe(testEvent)
  })

  it('should create and track own device invites', () => {
    const manager = new SessionManager(ourIdentityKey, nostrSubscribe, nostrPublish)
    
    const invite = manager.createOwnDeviceInvite('device-1', 'Test Device')
    expect(invite).toBeDefined()
    expect(invite.label).toBe('Test Device')
    
    const ownInvites = manager.getOwnDeviceInvites()
    expect(ownInvites.has('device-1')).toBe(true)
    expect(ownInvites.get('device-1')).toBe(invite)
  })

  it('should remove own device by nulling invite', () => {
    const manager = new SessionManager(ourIdentityKey, nostrSubscribe, nostrPublish)
    
    manager.createOwnDeviceInvite('device-1', 'Test Device')
    manager.removeOwnDevice('device-1')
    
    const ownInvites = manager.getOwnDeviceInvites()
    expect(ownInvites.get('device-1')).toBe(null)
    
    const activeInvites = manager.getActiveOwnDeviceInvites()
    expect(activeInvites.has('device-1')).toBe(false)
  })

  it('should filter out nulled invites from active invites', () => {
    const manager = new SessionManager(ourIdentityKey, nostrSubscribe, nostrPublish)
    
    manager.createOwnDeviceInvite('device-1', 'Device 1')
    manager.createOwnDeviceInvite('device-2', 'Device 2')
    manager.removeOwnDevice('device-1')
    
    const activeInvites = manager.getActiveOwnDeviceInvites()
    expect(activeInvites.size).toBe(1)
    expect(activeInvites.has('device-2')).toBe(true)
    expect(activeInvites.has('device-1')).toBe(false)
  })
})   