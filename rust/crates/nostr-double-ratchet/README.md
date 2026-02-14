# nostr-double-ratchet

Rust library implementing Double Ratchet messaging for Nostr, including multi-device session management and sender-key group messaging.

## Features

- 1:1 Double Ratchet sessions over Nostr events
- Invite/bootstrap flows for secure session establishment
- `SessionManager` for multi-device owner/device routing
- AppKeys-based device authorization and owner-claim validation
- Sender-key + one-to-many group messaging primitives
- Persistent storage adapters and message/discovery queues
- TypeScript/Rust interop test coverage

## Security Properties

### Confidentiality

- 1:1 payloads are encrypted with Double Ratchet and NIP-44.
- Group payloads are encrypted with sender-key chains and published as one-to-many outer events.

### Forward Secrecy And Post-Compromise Recovery

- 1:1 chains ratchet continuously, providing forward secrecy.
- Future secrecy recovers after fresh ratchet steps if transient compromise ends.

### Author/Device Verification

- Outer Nostr events are signature-verified.
- Identity attribution is based on authenticated session context and owner/device mapping.
- For multi-device owner claims, AppKeys are used to verify device authorization.
- Inner rumor `pubkey` is not treated as a trusted sender identity source.

### Plausible Deniability

- Inner rumors are unsigned payloads transported inside encrypted channels.
- This preserves deniability for inner content at the cost of strong non-repudiation.

## Group Messaging Architecture

Groups are handled with a hybrid model:

1. Membership is tracked by owner pubkeys.
2. Group metadata and sender-key distributions are sent over authenticated 1:1 sessions.
3. Each sender device uses a per-group sender-event keypair and sender-key state.
4. Group messages are published once (one-to-many), then decrypted by members with sender-key state.
5. Shared-channel events are used by higher-level integrations for signed bootstrap invites when pairwise sessions are missing.

## Basic 1:1 Usage

```rust
use nostr::Keys;
use nostr_double_ratchet::Session;

let alice_keys = Keys::generate();
let bob_keys = Keys::generate();

// Shared secret must come from a secure invite/bootstrap flow.
let shared_secret = [7u8; 32];

let mut alice = Session::init(
    bob_keys.public_key(),
    alice_keys.secret_key().to_secret_bytes(),
    true,
    shared_secret,
    Some("alice-chat".to_string()),
)?;

let mut bob = Session::init(
    alice_keys.public_key(),
    bob_keys.secret_key().to_secret_bytes(),
    false,
    shared_secret,
    Some("bob-chat".to_string()),
)?;

let outer = alice.send("hello bob".to_string())?;
let plaintext = bob.receive(&outer)?;
assert!(plaintext.is_some());
# Ok::<(), nostr_double_ratchet::Error>(())
```

## Disappearing Messages

Use NIP-40-style `["expiration", "<unix seconds>"]` tags in inner rumors.  
`SessionManager` helpers support global, per-peer, and per-group defaults through `SendOptions`.

## 1:1 Chat Settings Signaling

- Kind: `CHAT_SETTINGS_KIND = 10448`
- Content: `{ "type": "chat-settings", "v": 1, "messageTtlSeconds": <seconds|null> }`
- Settings events themselves should not expire

Receivers can auto-adopt or reject incoming settings policy.

## Testing

```bash
cargo test -p nostr-double-ratchet --manifest-path rust/Cargo.toml
```

For CLI/e2e coverage (including cross-language tests), run:

```bash
cargo test -p ndr --manifest-path rust/Cargo.toml
```
