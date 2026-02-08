# nostr-double-ratchet

End-to-end encrypted messaging for Nostr using the Double Ratchet algorithm.

## Installation

```bash
pnpm add nostr-double-ratchet
```

## Quick Start

```typescript
import { AppKeysManager, DelegateManager } from "nostr-double-ratchet"

// 1. Create device identity
const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage })
await delegate.init()

// 2. Register device (only on devices with main nsec)
const appKeysManager = new AppKeysManager({ nostrPublish, storage })
await appKeysManager.init()
appKeysManager.addDevice(delegate.getRegistrationPayload())
await appKeysManager.publish()

// 3. Activate and create session manager
await delegate.activate(ownerPublicKey)
const sessionManager = delegate.createSessionManager()
await sessionManager.init()

// 4. Send and receive messages
sessionManager.onEvent((event, from) => console.log(`${from}: ${event.content}`))
await sessionManager.sendMessage(recipientPubkey, "Hello!")
```

## Disappearing Messages (Expiration)

To send a disappearing message, include a NIP-40-style `["expiration", "<unix seconds>"]` tag in the *inner* rumor.
This library can do that for you:

```typescript
// Expires 60 seconds from now (using local time)
await sessionManager.sendMessage(recipientPubkey, "This will disappear", { ttlSeconds: 60 })

// Or set an absolute expiration timestamp (unix seconds)
await sessionManager.sendMessage(recipientPubkey, "Expires at a specific time", {
  expiresAt: 1704067260,
})

// Set defaults so you don't have to pass expiration on every send
await sessionManager.setDefaultExpiration({ ttlSeconds: 60 })
await sessionManager.setExpirationForPeer(recipientPubkey, { ttlSeconds: 120 })
await sessionManager.setExpirationForGroup(groupId, { ttlSeconds: 30 }) // applies when tags include ["l", groupId]

// Disable expiration for a peer/group even when a global default is set
await sessionManager.setExpirationForPeer(recipientPubkey, null)
await sessionManager.setExpirationForGroup(groupId, null)

// Disable expiration for a single send (even if defaults exist)
await sessionManager.sendMessage(recipientPubkey, "persist", { expiration: null })
```

This library does **not** delete old messages from storage; that must be implemented by the client/storage layer.
Decrypted expired rumors are still delivered to `onEvent`; clients can filter them (e.g. using `isExpired()`).

## Disappearing Message Signaling (1:1 Chat Settings)

To coordinate disappearing messages in a 1:1 chat, Iris uses an encrypted settings rumor:

- Kind: `CHAT_SETTINGS_KIND = 10448`
- Content JSON: `{ "type": "chat-settings", "v": 1, "messageTtlSeconds": <seconds|null> }`
- Settings events themselves do **not** expire.

The receiver auto-adopts the setting by default and updates their per-peer outgoing expiration.

```ts
// Set TTL for this peer and notify them (receiver auto-adopts)
await sessionManager.setChatSettingsForPeer(recipientPubkey, 60)

// Disable per-peer expiration even if you have a global default
await sessionManager.setChatSettingsForPeer(recipientPubkey, 0)

// Turn off auto-adopt if you want to require user confirmation
sessionManager.setAutoAdoptChatSettings(false)
```

## Multi-Device

- **Main device** (has nsec): Uses both `DelegateManager` and `AppKeysManager`
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
| AppKeys | 30078 | Lists authorized devices for a user |
| Invite | 30078 | Per-device keys for session establishment |
| Invite Response | 1059 | Encrypted session handshake |
