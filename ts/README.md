# nostr-double-ratchet

End-to-end encrypted messaging for Nostr using the Double Ratchet algorithm.

## Installation

```bash
pnpm add nostr-double-ratchet
```

## Quick Start

```typescript
import { ApplicationManager, DelegateManager } from "nostr-double-ratchet"

// 1. Create device identity
const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage })
await delegate.init()

// 2. Register device (only on devices with main nsec)
const applicationManager = new ApplicationManager({ nostrPublish, storage })
await applicationManager.init()
applicationManager.addDevice(delegate.getRegistrationPayload())
await applicationManager.publish()

// 3. Activate and create session manager
await delegate.activate(ownerPublicKey)
const sessionManager = delegate.createSessionManager()
await sessionManager.init()

// 4. Send and receive messages
sessionManager.onEvent((event, from) => console.log(`${from}: ${event.content}`))
await sessionManager.sendMessage(recipientPubkey, "Hello!")
```

## Multi-Device

- **Main device** (has nsec): Uses both `DelegateManager` and `ApplicationManager`
- **Delegate device** (no nsec): Uses only `DelegateManager`, waits for activation

```typescript
// Delegate device flow
const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage })
await delegate.init()
// Transfer delegate.getRegistrationPayload().identityPubkey to main device
const ownerPublicKey = await delegate.waitForActivation(60000)
const sessionManager = delegate.createSessionManager()
```

## Event Types

| Event | Kind | Purpose |
|-------|------|---------|
| ApplicationKeys | 30078 | Lists authorized devices for a user |
| Invite | 30078 | Per-device keys for session establishment |
| Invite Response | 1059 | Encrypted session handshake |
