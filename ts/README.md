# nostr-double-ratchet (TypeScript)

TypeScript implementation of end-to-end encrypted Nostr messaging using Double Ratchet, multi-device owner/device identity mapping, and sender-key-based group messaging.

## Installation

```bash
pnpm add nostr-double-ratchet
```

## Core Components

- `Session`: low-level 1:1 ratchet session
- `Invite`: handshake/bootstrap primitive
- `SessionManager`: multi-device session orchestration and routing
- `DelegateManager` / `AppKeysManager`: device lifecycle and owner authorization
- `Group`: sender-key group messaging helper (transport-agnostic)
- `SharedChannel`: encrypted shared-channel primitive used by higher-level group bootstrap flows

## Quick Start

```typescript
import { AppKeysManager, DelegateManager } from "nostr-double-ratchet"

// 1) Device identity
const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage })
await delegate.init()

// 2) Owner-authorized device list (on owner-authority device)
const appKeysManager = new AppKeysManager({ nostrPublish, storage })
await appKeysManager.init()
appKeysManager.addDevice(delegate.getRegistrationPayload())
await appKeysManager.publish()

// 3) Activate delegate and create SessionManager
await delegate.activate(ownerPublicKey)
const sessionManager = delegate.createSessionManager()
await sessionManager.init()

// 4) Send and receive
sessionManager.onEvent((event, from) => {
  console.log(`${from}: ${event.content}`)
})
await sessionManager.sendMessage(recipientPubkey, "Hello!")
```

## Security Properties

### Confidentiality

- 1:1 payloads are encrypted with Double Ratchet over NIP-44.
- Group payloads are encrypted with per-sender sender-key chains.

### Forward Secrecy And Post-Compromise Recovery

- 1:1 sessions get forward secrecy from ratcheting key evolution.
- Future secrecy recovers after new ratchet steps if a transient compromise ends.

### Author And Device Verification

- Outer events are signature-verified.
- Sender owner/device attribution comes from authenticated session context and AppKeys mappings.
- Multi-device owner claims are verified against AppKeys (not accepted blindly).
- Inner rumor `pubkey` should be treated as untrusted for identity decisions.

### Plausible Deniability

- Inner rumors are unsigned payloads carried in encrypted channels.
- You get channel authenticity, but not strong transferable non-repudiation of inner content.

## Groups

`Group` is transport-agnostic and implements efficient sender-key messaging:

- Membership is defined in owner pubkeys.
- Sender-key distributions are delivered pairwise over authenticated 1:1 sessions.
- Group messages are published once as one-to-many outer events (per sender device key).
- Receiver attribution for group payloads is derived from authenticated distribution/session context, not inner rumor `pubkey`.

## Disappearing Messages (Expiration)

Include NIP-40-style `["expiration", "<unix seconds>"]` on the inner rumor, or use helpers:

```typescript
await sessionManager.sendMessage(recipientPubkey, "expires soon", { ttlSeconds: 60 })
await sessionManager.setDefaultExpiration({ ttlSeconds: 60 })
await sessionManager.setExpirationForPeer(recipientPubkey, { ttlSeconds: 120 })
await sessionManager.setExpirationForGroup(groupId, { ttlSeconds: 30 })
await sessionManager.setExpirationForPeer(recipientPubkey, null)
await sessionManager.sendMessage(recipientPubkey, "persist", { expiration: null })
```

The library does not purge local storage for you. Clients should enforce retention/UI behavior.

## 1:1 Chat Settings Signaling

Encrypted settings rumor kind:

- `CHAT_SETTINGS_KIND = 10448`
- Content: `{ "type": "chat-settings", "v": 1, "messageTtlSeconds": <seconds|null> }`
- Settings events themselves do not expire

```ts
await sessionManager.setChatSettingsForPeer(recipientPubkey, 60)
await sessionManager.setChatSettingsForPeer(recipientPubkey, 0)
sessionManager.setAutoAdoptChatSettings(false)
```

## Event Kinds

| Kind | Constant | Purpose |
|------|----------|---------|
| 1060 | `MESSAGE_EVENT_KIND` | Encrypted outer event |
| 30078 | `INVITE_EVENT_KIND` / `APP_KEYS_EVENT_KIND` | Device invite and AppKeys records |
| 1059 | `INVITE_RESPONSE_KIND` | Encrypted invite response |
| 14 | `CHAT_MESSAGE_KIND` | Inner chat message rumor |
| 10448 | `CHAT_SETTINGS_KIND` | Inner chat-settings rumor |
| 40 | `GROUP_METADATA_KIND` | Group metadata rumor |
| 10445 | `GROUP_INVITE_RUMOR_KIND` | Group bootstrap invite rumor |
| 10446 | `GROUP_SENDER_KEY_DISTRIBUTION_KIND` | Group sender-key distribution rumor |
| 10447 | `GROUP_SENDER_KEY_MESSAGE_KIND` | Group sender-key message rumor kind constant |
| 4 | `SHARED_CHANNEL_KIND` | Shared-channel transport event kind |

## Scalability And Tradeoffs

- Group steady-state publish is O(1) per message (single outer event).
- Sender-key distribution and metadata fanout are O(members/devices), so membership changes are the expensive path.
- Multi-device support improves UX but increases session/subscription/state complexity.
- Relay-level metadata (timing, pubkeys, traffic patterns) remains visible.

## Development

```bash
pnpm -C ts test:once
```
