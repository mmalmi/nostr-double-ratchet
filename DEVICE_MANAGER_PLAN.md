# DeviceManager Plan

## Goal

Create a `DeviceManager` class that handles all device-related concerns, lifting this responsibility from `SessionManager`. This creates a cleaner separation:

- **DeviceManager** - Device lifecycle (InviteList, add/revoke devices, delegate activation)
- **SessionManager** - Messaging only (sessions, send/receive messages)

## Architecture

```
DeviceManager
├── Owns InviteList (publish, subscribe, modify)
├── Add/revoke/update devices
├── Delegate mode: wait for activation, check revocation
├── Provides device info to SessionManager
└── No messaging logic

SessionManager
├── Owns sessions (create, store, rotate)
├── Send/receive messages via double ratchet
├── Listens for invite responses (session establishment)
└── No device management logic
```

## DeviceManager API

```typescript
interface DeviceManagerOptions {
  // For main device (has nsec)
  ownerPublicKey?: string
  ownerPrivateKey?: Uint8Array

  // For delegate device (no nsec)
  delegateMode?: boolean
  devicePublicKey?: string      // Delegate's own identity pubkey
  devicePrivateKey?: Uint8Array // Delegate's own identity privkey
  ephemeralPublicKey?: string
  ephemeralPrivateKey?: Uint8Array
  sharedSecret?: string

  // Common
  deviceId: string
  deviceLabel: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
  storage?: StorageAdapter
}

class DeviceManager {
  // Factory methods
  static createMain(options: MainDeviceOptions): DeviceManager
  static createDelegate(options: DelegateDeviceOptions): { manager: DeviceManager, payload: DevicePayload }

  // Lifecycle
  async init(): Promise<void>
  close(): void

  // Mode detection
  isDelegateMode(): boolean

  // Device management (main device only)
  async addDevice(payload: DevicePayload): Promise<void>
  async revokeDevice(deviceId: string): Promise<void>
  async updateDeviceLabel(deviceId: string, label: string): Promise<void>
  getOwnDevices(): DeviceEntry[]

  // Delegate-specific
  async waitForActivation(timeoutMs?: number): Promise<string>  // Returns owner pubkey
  getOwnerPublicKey(): string | null
  async isRevoked(): Promise<boolean>

  // For SessionManager integration
  getInviteList(): InviteList | null
  getDeviceId(): string
  getIdentityPublicKey(): string   // Owner pubkey (main) or device pubkey (delegate)
  getIdentityPrivateKey(): Uint8Array
  getEphemeralKeypair(): { publicKey: string, privateKey: Uint8Array } | null
  getSharedSecret(): string | null
}
```

## SessionManager Changes

Remove from SessionManager:
- `addDevice()`
- `revokeDevice()`
- `updateDeviceLabel()`
- `getOwnDevices()`
- `getOwnDevice()`
- All delegate mode logic
- InviteList ownership

SessionManager becomes simpler:
```typescript
class SessionManager {
  constructor(
    identityPublicKey: string,
    identityPrivateKey: Uint8Array | DecryptFunction,
    deviceId: string,
    nostrSubscribe: NostrSubscribe,
    nostrPublish: NostrPublish,
    storage?: StorageAdapter,
    // Optional: for listening to invite responses
    ephemeralKeypair?: { publicKey: string, privateKey: Uint8Array },
    sharedSecret?: string
  )

  async init(): Promise<void>
  close(): void

  // User/session management
  setupUser(userPubkey: string): void
  deleteUser(userPubkey: string): Promise<void>

  // Messaging
  sendMessage(recipientPublicKey: string, content: string, options?: MessageOptions): Promise<Rumor>
  sendEvent(recipientIdentityKey: string, event: Partial<Rumor>): Promise<Rumor | undefined>
  onEvent(callback: OnEventCallback): () => void

  // Session info
  getDeviceId(): string
  getUserRecords(): Map<string, UserRecord>
}
```

## Integration Pattern

```typescript
// Main device usage
const deviceManager = DeviceManager.createMain({
  ownerPublicKey: pubkey,
  ownerPrivateKey: privkey,
  deviceId: 'main-device',
  deviceLabel: 'Main Device',
  nostrSubscribe,
  nostrPublish,
  storage
})

await deviceManager.init()

const sessionManager = new SessionManager(
  deviceManager.getIdentityPublicKey(),
  deviceManager.getIdentityPrivateKey(),
  deviceManager.getDeviceId(),
  nostrSubscribe,
  nostrPublish,
  storage,
  deviceManager.getEphemeralKeypair(),
  deviceManager.getSharedSecret()
)

await sessionManager.init()

// Add a delegate device
await deviceManager.addDevice(delegatePayload)


// Delegate device usage
const { manager: deviceManager, payload } = DeviceManager.createDelegate({
  deviceId: 'phone-123',
  deviceLabel: 'My Phone',
  nostrSubscribe,
  nostrPublish,
  storage
})

// Display payload as QR code...

await deviceManager.init()
const ownerPubkey = await deviceManager.waitForActivation()

const sessionManager = new SessionManager(
  deviceManager.getIdentityPublicKey(),
  deviceManager.getIdentityPrivateKey(),
  deviceManager.getDeviceId(),
  nostrSubscribe,
  nostrPublish,
  storage,
  deviceManager.getEphemeralKeypair(),
  deviceManager.getSharedSecret()
)

await sessionManager.init()
```

## Test Plan (TDD)

### Phase 1: DeviceManager - Main Device Mode

```typescript
// tests/DeviceManager.main.test.ts

describe('DeviceManager - Main Device', () => {
  describe('createMain()', () => {
    it('should create a DeviceManager in main mode')
    it('should return isDelegateMode() === false')
  })

  describe('init()', () => {
    it('should create InviteList with own device')
    it('should publish InviteList on init')
    it('should load existing InviteList from storage')
    it('should merge local and remote InviteLists')
  })

  describe('addDevice()', () => {
    it('should add device to InviteList')
    it('should publish updated InviteList')
    it('should include identityPubkey for delegate devices')
  })

  describe('revokeDevice()', () => {
    it('should remove device from InviteList')
    it('should publish updated InviteList')
    it('should not allow revoking own device')
  })

  describe('updateDeviceLabel()', () => {
    it('should update device label in InviteList')
    it('should publish updated InviteList')
  })

  describe('getOwnDevices()', () => {
    it('should return all devices from InviteList')
  })

  describe('getters', () => {
    it('getIdentityPublicKey() should return owner pubkey')
    it('getIdentityPrivateKey() should return owner privkey')
    it('getDeviceId() should return device ID')
    it('getEphemeralKeypair() should return ephemeral keys')
    it('getSharedSecret() should return shared secret')
    it('getInviteList() should return InviteList')
  })
})
```

### Phase 2: DeviceManager - Delegate Mode

```typescript
// tests/DeviceManager.delegate.test.ts

describe('DeviceManager - Delegate Device', () => {
  describe('createDelegate()', () => {
    it('should create a DeviceManager in delegate mode')
    it('should return isDelegateMode() === true')
    it('should generate identity keypair')
    it('should generate ephemeral keypair')
    it('should generate shared secret')
    it('should return payload with public keys')
  })

  describe('init()', () => {
    it('should NOT publish InviteList')
    it('should load stored owner pubkey if exists')
  })

  describe('waitForActivation()', () => {
    it('should subscribe to InviteList events')
    it('should resolve when own deviceId appears in an InviteList')
    it('should return the owner pubkey who added this device')
    it('should store owner pubkey for future use')
    it('should timeout if not activated within timeoutMs')
    it('should resolve immediately if already activated')
  })

  describe('getOwnerPublicKey()', () => {
    it('should return null before activation')
    it('should return owner pubkey after activation')
  })

  describe('isRevoked()', () => {
    it('should return false when device is in InviteList')
    it('should return true when device is removed from InviteList')
  })

  describe('restrictions', () => {
    it('addDevice() should throw in delegate mode')
    it('revokeDevice() should throw in delegate mode')
    it('updateDeviceLabel() should throw in delegate mode')
  })

  describe('getters', () => {
    it('getIdentityPublicKey() should return delegate device pubkey')
    it('getIdentityPrivateKey() should return delegate device privkey')
  })
})
```

### Phase 3: Integration Tests

```typescript
// tests/DeviceManager.integration.test.ts

describe('DeviceManager Integration', () => {
  it('main device adds delegate, delegate activates', async () => {
    // 1. Create delegate device manager
    const { manager: delegateManager, payload } = DeviceManager.createDelegate(...)

    // 2. Create main device manager
    const mainManager = DeviceManager.createMain(...)
    await mainManager.init()

    // 3. Main adds delegate
    await mainManager.addDevice(payload)

    // 4. Delegate waits for activation
    await delegateManager.init()
    const ownerPubkey = await delegateManager.waitForActivation()

    // 5. Verify
    expect(ownerPubkey).toBe(mainManager.getIdentityPublicKey())
    expect(delegateManager.getOwnerPublicKey()).toBe(ownerPubkey)
  })

  it('main device revokes delegate, delegate detects revocation', async () => {
    // Setup: main adds delegate, delegate activates
    // ...

    // Main revokes delegate
    await mainManager.revokeDevice(payload.deviceId)

    // Delegate checks revocation
    const revoked = await delegateManager.isRevoked()
    expect(revoked).toBe(true)
  })

  it('DeviceManager + SessionManager work together', async () => {
    // 1. Setup main device
    const mainDeviceManager = DeviceManager.createMain(...)
    await mainDeviceManager.init()

    const mainSessionManager = new SessionManager(
      mainDeviceManager.getIdentityPublicKey(),
      mainDeviceManager.getIdentityPrivateKey(),
      mainDeviceManager.getDeviceId(),
      ...
    )
    await mainSessionManager.init()

    // 2. Setup delegate device
    const { manager: delegateDeviceManager, payload } = DeviceManager.createDelegate(...)
    await mainDeviceManager.addDevice(payload)
    await delegateDeviceManager.init()
    await delegateDeviceManager.waitForActivation()

    const delegateSessionManager = new SessionManager(
      delegateDeviceManager.getIdentityPublicKey(),
      delegateDeviceManager.getIdentityPrivateKey(),
      delegateDeviceManager.getDeviceId(),
      ...
    )
    await delegateSessionManager.init()

    // 3. Setup external user
    const userSessionManager = new SessionManager(userPubkey, userPrivkey, ...)
    await userSessionManager.init()

    // 4. User sends message to owner (should reach both devices)
    userSessionManager.setupUser(mainDeviceManager.getIdentityPublicKey())
    await userSessionManager.sendMessage(mainDeviceManager.getIdentityPublicKey(), 'Hello')

    // 5. Both devices should receive the message
    // (via their respective SessionManagers)
  })
})
```

## Implementation Order

1. Write tests for Phase 1 (Main Device Mode)
2. Implement DeviceManager main mode to pass tests
3. Write tests for Phase 2 (Delegate Mode)
4. Implement DeviceManager delegate mode to pass tests
5. Write tests for Phase 3 (Integration)
6. Refactor SessionManager to remove device management
7. Update exports in index.ts
8. Update client code to use new API

## Files to Create/Modify

| File | Action |
|------|--------|
| `src/DeviceManager.ts` | CREATE - new class |
| `tests/DeviceManager.main.test.ts` | CREATE - main mode tests |
| `tests/DeviceManager.delegate.test.ts` | CREATE - delegate mode tests |
| `tests/DeviceManager.integration.test.ts` | CREATE - integration tests |
| `src/SessionManager.ts` | MODIFY - remove device management |
| `src/index.ts` | MODIFY - export DeviceManager |
