# nostr-double-ratchet

End-to-end encrypted messaging for Nostr using the Double Ratchet algorithm.

## Installation

```bash
pnpm add nostr-double-ratchet
```

## Device Setup

All devices need two things:
- **DelegateManager**: Device identity (all devices use this)
- **DeviceManager**: InviteList authority (only devices with main nsec)

### Use Case 1: First-Time Setup (New User)

Main device initializes messaging for the first time.

```typescript
import { DeviceManager, DelegateManager } from "nostr-double-ratchet"

// Create device identity
const delegate = new DelegateManager({
  nostrSubscribe,
  nostrPublish,
  storage,
})
await delegate.init()
const payload = delegate.getRegistrationPayload()

// Create InviteList authority and add ourselves
const deviceManager = new DeviceManager({ nostrPublish, storage })
await deviceManager.init()
deviceManager.addDevice(payload)
await deviceManager.publish()

// Activate (we know the owner - it's us)
await delegate.activate(ownerPublicKey)

// Create SessionManager for messaging
const sessionManager = delegate.createSessionManager()
await sessionManager.init()
```

### Use Case 2: Adding Another Device (With Main nsec)

User logs in on a new device using their main Nostr secret key.

```typescript
import { DeviceManager, DelegateManager, InviteList } from "nostr-double-ratchet"

// Create device identity for this device
const delegate = new DelegateManager({
  nostrSubscribe,
  nostrPublish,
  storage,
})
await delegate.init()
const payload = delegate.getRegistrationPayload()

// Create InviteList authority
const deviceManager = new DeviceManager({ nostrPublish, storage })
await deviceManager.init()

// Fetch existing InviteList from relays and merge
const existing = await InviteList.waitFor(ownerPublicKey, nostrSubscribe, 2000)
if (existing) {
  await deviceManager.setInviteList(existing)
}

// Add this device and publish
deviceManager.addDevice(payload)
await deviceManager.publish()

// Activate and create SessionManager
await delegate.activate(ownerPublicKey)
const sessionManager = delegate.createSessionManager()
await sessionManager.init()
```

### Use Case 3: Delegate-Only Device (No Main nsec)

A secondary device that doesn't have authority over the InviteList. Requires coordination with a main device.

#### Step 1: Create and Initialize Device Identity

On the new delegate device:

```typescript
import { DelegateManager } from "nostr-double-ratchet"

const delegate = new DelegateManager({
  nostrSubscribe,
  nostrPublish,
  storage,
})
await delegate.init()
const payload = delegate.getRegistrationPayload()

// payload = { identityPubkey: "abc123..." }
```

#### Step 2: Transfer Payload to Main Device

Display `payload.identityPubkey` to user via QR code, copy-paste, NFC, etc.

```typescript
console.log("Add this device on your main device:", payload.identityPubkey)
```

#### Step 3: Main Device Adds Delegate

On the main device (which has `DeviceManager`):

```typescript
const delegatePayload = { identityPubkey: "abc123..." }

deviceManager.addDevice(delegatePayload)
await deviceManager.publish()
```

#### Step 4: Wait for Activation

Back on the delegate device:

```typescript
const ownerPublicKey = await delegate.waitForActivation(60000)
// Subscribes to InviteList events until it finds one containing its identityPubkey
// Returns the owner's pubkey (the InviteList author)
```

#### Step 5: Create SessionManager

```typescript
const sessionManager = delegate.createSessionManager()
await sessionManager.init()
```

#### Complete Delegate Device Code

```typescript
import { DelegateManager } from "nostr-double-ratchet"

// 1. Create and initialize
const delegate = new DelegateManager({
  nostrSubscribe,
  nostrPublish,
  storage,
})
await delegate.init()
const payload = delegate.getRegistrationPayload()

// 2. Show to user for transfer to main device
displayQRCode(payload.identityPubkey)

// 3. Wait for main device to add us
const ownerPublicKey = await delegate.waitForActivation(60000)

// 4. Create SessionManager
const sessionManager = delegate.createSessionManager()
await sessionManager.init()
```

## Sending and Receiving Messages

Once you have a SessionManager:

```typescript
// Listen for incoming messages
sessionManager.onEvent((event, from) => {
  console.log(`Message from ${from}:`, event.content)
})

// Send a message
await sessionManager.sendMessage(recipientPubkey, "Hello!")
```

## Automatic Key Persistence

Identity keys are automatically stored in your `StorageAdapter` and restored on restart:

```typescript
// First run - generates new keys
const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage })
await delegate.init()
// Keys saved to storage automatically

// After restart - same storage = keys restored automatically
const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage })
await delegate.init()
// Same identity keys, no manual persistence needed
```

## Architecture Overview

### Event Types

| Event | Kind | Purpose |
|-------|------|---------|
| InviteList | 30078 (d: "double-ratchet/invite-list") | Lists all devices for a user |
| Invite | 30078 (d: "double-ratchet/invite/{id}") | Per-device ephemeral keys for DH |
| Invite Response | 1059 | Encrypted session establishment |

### Key Concepts

- **InviteList**: Published by owner, contains `identityPubkey` for each authorized device
- **Invite**: Published by each device, contains ephemeral keys and shared secret for session establishment
- **identityPubkey**: Serves as both device identity and device ID
- **ownerPublicKey**: The user's main Nostr pubkey (npub)

### Session Establishment Flow

1. Alice's device publishes InviteList with her devices
2. Each of Alice's devices publishes its own Invite
3. Bob wants to message Alice:
   - Fetches Alice's InviteList
   - For each device, fetches its Invite
   - Accepts invite, creating encrypted session
4. Alice's devices receive invite responses and establish sessions
5. Messages flow through double-ratchet encrypted channels

## API Reference

### DelegateManager

All devices use this for identity.

```typescript
// Create
new DelegateManager(options)

// Methods
delegate.init(): Promise<void>                    // Loads or generates keys
delegate.getRegistrationPayload(): DelegatePayload // Get payload for DeviceManager
delegate.getIdentityPublicKey(): string
delegate.getIdentityKey(): Uint8Array
delegate.getOwnerPublicKey(): string | null
delegate.activate(ownerPublicKey): Promise<void>
delegate.waitForActivation(timeoutMs?): Promise<string>
delegate.rotateInvite(): Promise<void>
delegate.isRevoked(): Promise<boolean>
delegate.createSessionManager(storage?): SessionManager
delegate.close(): void
```

### DeviceManager

Only for devices with main nsec (InviteList authority).

```typescript
// Create
new DeviceManager(options)

// Methods
deviceManager.init(): Promise<void>
deviceManager.addDevice(payload): void           // Local only
deviceManager.revokeDevice(identityPubkey): void // Local only
deviceManager.publish(): Promise<void>           // Publishes to relays
deviceManager.setInviteList(list): Promise<void> // For authority transfer
deviceManager.getInviteList(): InviteList | null
deviceManager.getAllDevices(): DeviceEntry[]
deviceManager.close(): void
```

### SessionManager

Handles encrypted messaging.

```typescript
// Created via DelegateManager
const sessionManager = delegate.createSessionManager(storage?)

// Methods
sessionManager.init(): Promise<void>
sessionManager.sendMessage(recipient, content, options?): Promise<Rumor>
sessionManager.sendEvent(recipient, event): Promise<Rumor | undefined>
sessionManager.onEvent(callback): Unsubscribe
sessionManager.setupUser(userPubkey): void
sessionManager.deleteUser(userPubkey): Promise<void>
sessionManager.close(): void
```
