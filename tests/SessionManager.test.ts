import { describe, it, expect, vi } from 'vitest'
import SessionManager from '../src/SessionManager'
import { generateSecretKey, getPublicKey } from 'nostr-tools'
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
  const nostrPublish = vi.fn().mockResolvedValue({} as any)
  const ourIdentityKey = generateSecretKey()
  const deviceId = 'test-device'

  it('should start listening and throw when no active session exists', async () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const listenSpy = vi.spyOn(manager as any, 'listenToUser')

    const result = await manager.sendText('recipient', 'hello')
    expect(result).toEqual([])
    expect(listenSpy).toHaveBeenCalledWith('recipient')
  })

  it('should send events to all active sessions', async () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)

    const recipient = 'recipientPubKey'
    const session = createStubSession()
    const userRecord = new UserRecord(recipient, nostrSubscribe)
    userRecord.upsertSession(undefined, session)
    ;(manager as any).userRecords.set(recipient, userRecord)

    const results = await manager.sendText(recipient, 'hello')

    expect(session.sendEvent).toHaveBeenCalledTimes(1)
    expect(session.sendEvent).toHaveBeenCalledWith({ kind: CHAT_MESSAGE_KIND, content: 'hello' })
    expect(results).toHaveLength(1)
  })

  it('should propagate incoming session events to listeners', () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)

    const recipient = 'recipientPubKey'
    const session = createStubSession()
    const userRecord = new UserRecord(recipient, nostrSubscribe)
    userRecord.upsertSession(undefined, session)
    ;(manager as any).userRecords.set(recipient, userRecord)

    const received: any[] = []
    manager.onEvent((e) => received.push(e))

    const testEvent = { content: 'incoming' }
    ;(session as any)._emit(testEvent)
    expect(received).toHaveLength(1)
    expect(received[0]).toBe(testEvent)
  })

  it('should create and track own device sessions', () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)
    
    // Create a session for our own device
    const session = createStubSession()
    const userRecord = new UserRecord(ourPublicKey, nostrSubscribe)
    userRecord.upsertSession(undefined, session)
    ;(manager as any).userRecords.set(ourPublicKey, userRecord)

    // Verify the session is tracked
    const record = (manager as any).userRecords.get(ourPublicKey)
    expect(record).toBeDefined()
    expect(record.getActiveSessions()).toContain(session)
  })

  it('should remove own device session', () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)
    
    // Create a session for our own device
    const session = createStubSession()
    const userRecord = new UserRecord(ourPublicKey, nostrSubscribe)
    userRecord.upsertSession(undefined, session)
    ;(manager as any).userRecords.set(ourPublicKey, userRecord)

    // Close the session
    session.close()

    // Verify the session is still tracked (since it's in extraSessions)
    const record = (manager as any).userRecords.get(ourPublicKey)
    expect(record.getActiveSessions()).toContain(session)
  })

  it('should track multiple own device sessions', () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)

    // Create sessions for two of our devices
    const session1 = createStubSession()
    const session2 = createStubSession()
    const userRecord = new UserRecord(ourPublicKey, nostrSubscribe)
    userRecord.upsertSession('device-1', session1)
    userRecord.upsertSession('device-2', session2)
    ;(manager as any).userRecords.set(ourPublicKey, userRecord)

    // Verify both sessions are tracked as active (one per device)
    const record = (manager as any).userRecords.get(ourPublicKey)
    expect(record.getActiveSessions()).toContain(session1)
    expect(record.getActiveSessions()).toContain(session2)
    expect(record.getActiveSessions()).toHaveLength(2)
  })

  it('should send events to both recipient and own devices for multi-device sync', async () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)
    
    const recipient = 'recipientPubKey'
    const recipientSession = createStubSession()
    const recipientRecord = new UserRecord(recipient, nostrSubscribe)
    recipientRecord.upsertSession('recipient-device', recipientSession)
    ;(manager as any).userRecords.set(recipient, recipientRecord)
    
    const ownSession1 = createStubSession()
    const ownSession2 = createStubSession()
    const ownRecord = new UserRecord(ourPublicKey, nostrSubscribe)
    ownRecord.upsertSession('own-device-1', ownSession1)
    ownRecord.upsertSession('own-device-2', ownSession2)
    ;(manager as any).userRecords.set(ourPublicKey, ownRecord)
    
    const results = await manager.sendEvent(recipient, { content: 'test message' })
    
    expect(recipientSession.sendEvent).toHaveBeenCalledWith({ content: 'test message' })
    
    expect(ownSession1.sendEvent).toHaveBeenCalledWith({ content: 'test message' })
    expect(ownSession2.sendEvent).toHaveBeenCalledWith({ content: 'test message' })
    
    expect(results).toHaveLength(3)
  })

  it('should establish sessions with own devices via invite acceptance', async () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)
    
    // Simulate receiving an invite from our own device
    const mockInvite = {
      deviceId: 'other-device',
      accept: vi.fn().mockResolvedValue({
        session: createStubSession(),
        event: { id: 'acceptance-event' }
      })
    }
    
    await manager.acceptOwnInvite(mockInvite as any)
    
    // Verify session was created and stored
    const ownRecord = (manager as any).userRecords.get(ourPublicKey)
    expect(ownRecord).toBeDefined()
    expect(ownRecord.getActiveSessions()).toHaveLength(1)
    expect(mockInvite.accept).toHaveBeenCalled()
  })

  it('should propagate messages between own devices', () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)
    
    const ownSession1 = createStubSession()
    const ownSession2 = createStubSession()
    const ownRecord = new UserRecord(ourPublicKey, nostrSubscribe)
    ownRecord.upsertSession('device-1', ownSession1)
    ownRecord.upsertSession('device-2', ownSession2)
    ;(manager as any).userRecords.set(ourPublicKey, ownRecord)
    
    const received: any[] = []
    manager.onEvent((e) => received.push(e))
    
    // Simulate message from device-1
    const testEvent = { content: 'message from device-1' }
    ;(ownSession1 as any)._emit(testEvent)
    
    expect(received).toHaveLength(1)
    expect(received[0]).toBe(testEvent)
  })

  it('should send to own devices even when no recipient session exists', async () => {
    const manager = new SessionManager(ourIdentityKey, deviceId, nostrSubscribe, nostrPublish)
    const ourPublicKey = getPublicKey(ourIdentityKey)
    const listenSpy = vi.spyOn(manager as any, 'listenToUser')
    
    const ownSession1 = createStubSession()
    const ownSession2 = createStubSession()
    const ownRecord = new UserRecord(ourPublicKey, nostrSubscribe)
    ownRecord.upsertSession('own-device-1', ownSession1)
    ownRecord.upsertSession('own-device-2', ownSession2)
    ;(manager as any).userRecords.set(ourPublicKey, ownRecord)
    
    const results = await manager.sendEvent('nonexistent-recipient', { content: 'test message' })
    
    expect(listenSpy).toHaveBeenCalledWith('nonexistent-recipient')
    
    expect(ownSession1.sendEvent).toHaveBeenCalledWith({ content: 'test message' })
    expect(ownSession2.sendEvent).toHaveBeenCalledWith({ content: 'test message' })
    
    expect(results).toHaveLength(2)
  })
})               