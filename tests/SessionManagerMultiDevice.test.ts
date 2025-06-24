import { describe, it, expect, vi } from 'vitest'
import SessionManager from '../src/SessionManager'
import { generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools'
import { makeSubscribe, publish } from './helpers/mockRelay'
import { InMemoryStorageAdapter } from '../src/StorageAdapter'

describe('SessionManager Multi-Device Integration', () => {
  it('should sync messages between own devices via SessionManager', async () => {
    const storage = new InMemoryStorageAdapter()
    const subscribe = makeSubscribe() as any
    
    const alicePriv = generateSecretKey()
    const alicePub = getPublicKey(alicePriv)
    
    const signAndPublish = vi.fn((unsigned: any) => {
      const signed = finalizeEvent(unsigned, alicePriv)
      return publish(signed as any) as any
    })
    
    const aliceDevice1 = new SessionManager(alicePriv, 'alice-device-1', subscribe, signAndPublish, storage)
    const aliceDevice2 = new SessionManager(alicePriv, 'alice-device-2', subscribe, signAndPublish, storage)
    
    await aliceDevice1.init()
    await aliceDevice2.init()
    
    await new Promise(r => setTimeout(r, 500))
    
    const device1Messages: any[] = []
    const device2Messages: any[] = []
    
    aliceDevice1.onEvent((event) => device1Messages.push(event))
    aliceDevice2.onEvent((event) => device2Messages.push(event))
    
    const friendPubKey = getPublicKey(generateSecretKey())
    await aliceDevice1.sendText(friendPubKey, 'Hello from device 1')
    
    await new Promise(r => setTimeout(r, 300))
    
    expect(device2Messages.some(msg => msg.content?.includes('Hello from device 1'))).toBe(true)
    
    aliceDevice1.close()
    aliceDevice2.close()
  })

  it('should establish sessions between own devices automatically', async () => {
    const storage = new InMemoryStorageAdapter()
    const subscribe = makeSubscribe() as any
    
    const alicePriv = generateSecretKey()
    const alicePub = getPublicKey(alicePriv)
    
    const signAndPublish = vi.fn((unsigned: any) => {
      const signed = finalizeEvent(unsigned, alicePriv)
      return publish(signed as any) as any
    })
    
    const aliceDevice1 = new SessionManager(alicePriv, 'alice-device-1', subscribe, signAndPublish, storage)
    await aliceDevice1.init()
    
    const aliceDevice2 = new SessionManager(alicePriv, 'alice-device-2', subscribe, signAndPublish, storage)
    await aliceDevice2.init()
    
    await new Promise(r => setTimeout(r, 500))
    
    const device1Record = (aliceDevice1 as any).userRecords.get(alicePub)
    const device2Record = (aliceDevice2 as any).userRecords.get(alicePub)
    
    expect(device1Record).toBeDefined()
    expect(device2Record).toBeDefined()
    expect(device1Record.getActiveSessions().length).toBeGreaterThan(0)
    expect(device2Record.getActiveSessions().length).toBeGreaterThan(0)
    
    aliceDevice1.close()
    aliceDevice2.close()
  })

  it('should handle multiple own devices with message broadcasting', async () => {
    const storage = new InMemoryStorageAdapter()
    const subscribe = makeSubscribe() as any
    
    const alicePriv = generateSecretKey()
    const alicePub = getPublicKey(alicePriv)
    
    const signAndPublish = vi.fn((unsigned: any) => {
      const signed = finalizeEvent(unsigned, alicePriv)
      return publish(signed as any) as any
    })
    
    const aliceDevice1 = new SessionManager(alicePriv, 'alice-device-1', subscribe, signAndPublish, storage)
    const aliceDevice2 = new SessionManager(alicePriv, 'alice-device-2', subscribe, signAndPublish, storage)
    const aliceDevice3 = new SessionManager(alicePriv, 'alice-device-3', subscribe, signAndPublish, storage)
    
    await aliceDevice1.init()
    await aliceDevice2.init()
    await aliceDevice3.init()
    
    await new Promise(r => setTimeout(r, 800))
    
    const device1Messages: any[] = []
    const device2Messages: any[] = []
    const device3Messages: any[] = []
    
    aliceDevice1.onEvent((event) => device1Messages.push(event))
    aliceDevice2.onEvent((event) => device2Messages.push(event))
    aliceDevice3.onEvent((event) => device3Messages.push(event))
    
    const friendPubKey = getPublicKey(generateSecretKey())
    await aliceDevice2.sendText(friendPubKey, 'Message from device 2')
    
    await new Promise(r => setTimeout(r, 400))
    
    expect(device1Messages.some(msg => msg.content?.includes('Message from device 2'))).toBe(true)
    expect(device3Messages.some(msg => msg.content?.includes('Message from device 2'))).toBe(true)
    
    aliceDevice1.close()
    aliceDevice2.close()
    aliceDevice3.close()
  })

  it('should persist own device sessions across restarts', async () => {
    const storage = new InMemoryStorageAdapter()
    const subscribe = makeSubscribe() as any
    
    const alicePriv = generateSecretKey()
    const alicePub = getPublicKey(alicePriv)
    
    const signAndPublish = vi.fn((unsigned: any) => {
      const signed = finalizeEvent(unsigned, alicePriv)
      return publish(signed as any) as any
    })
    
    const aliceDevice1 = new SessionManager(alicePriv, 'alice-device-1', subscribe, signAndPublish, storage)
    await aliceDevice1.init()
    
    const aliceDevice2 = new SessionManager(alicePriv, 'alice-device-2', subscribe, signAndPublish, storage)
    await aliceDevice2.init()
    
    await new Promise(r => setTimeout(r, 500))
    
    aliceDevice1.close()
    
    const aliceDevice1Restarted = new SessionManager(alicePriv, 'alice-device-1', subscribe, signAndPublish, storage)
    await aliceDevice1Restarted.init()
    
    await new Promise(r => setTimeout(r, 300))
    
    const restartedRecord = (aliceDevice1Restarted as any).userRecords.get(alicePub)
    expect(restartedRecord).toBeDefined()
    expect(restartedRecord.getActiveSessions().length).toBeGreaterThan(0)
    
    aliceDevice1Restarted.close()
    aliceDevice2.close()
  })
})
