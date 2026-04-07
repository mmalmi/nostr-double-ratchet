# ndr-ffi

UniFFI bindings for `nostr-double-ratchet` - enables iOS and Android apps to use the Nostr Double Ratchet protocol.

## Overview

This crate provides FFI-friendly wrappers around the core `nostr-double-ratchet` library. All keys and events are represented as hex/JSON strings for easy interop with mobile platforms.

## API

### Key Generation

```rust
let keypair = generate_keypair();
// keypair.public_key_hex - 64 char hex string
// keypair.private_key_hex - 64 char hex string
```

### Invite Handling

```rust
// Create invite
let invite = InviteHandle::create_new(pubkey_hex, device_id, max_uses)?;
let url = invite.to_url("https://myapp.com")?;

// Accept invite
let invite = InviteHandle::from_url(url)?;
let result = invite.accept(invitee_pubkey_hex, invitee_privkey_hex, device_id)?;
// result.session - ready to send messages
// result.response_event_json - publish to relays

// Serialize/deserialize for storage
let json = invite.serialize()?;
let invite = InviteHandle::deserialize(json)?;
```

### Session Messaging

```rust
// Send message
let result = session.send_text("Hello!")?;
// result.outer_event_json - encrypted event to publish
// result.inner_event_json - original message as event

// Receive message
let result = session.decrypt_event(event_json)?;
// result.plaintext - decrypted message
// result.inner_event_json - inner event if JSON

// Serialize/deserialize for storage
let state = session.state_json()?;
let session = SessionHandle::from_state_json(state)?;
```

### Shared Multi-Device Helpers

These wrappers are intended to keep mobile clients on the same policy as the core library:

```rust
let devices = resolve_latest_app_keys_devices(app_keys_event_jsons)?;
let candidates = resolve_conversation_candidate_pubkeys(
    owner_pubkey_hex,
    rumor_pubkey_hex,
    rumor_tags,
    sender_pubkey_hex,
);
```

- `resolve_latest_app_keys_devices(...)` converges a set of AppKeys events into the latest
  monotonic authorized-device view.
- `resolve_conversation_candidate_pubkeys(...)` returns the ordered conversation candidates for a
  decrypted rumor, including self-DM and linked-device cases.

### SessionManager Inspection

`SessionManagerHandle` now exposes supported inspection APIs so mobile apps do not need to read
`user_<peer>.json` files directly:

```rust
let manager = SessionManagerHandle::new_with_storage_path(
    our_pubkey_hex,
    our_identity_privkey_hex,
    device_id,
    storage_path,
    owner_pubkey_hex,
)?;
manager.init()?;

let peers = manager.known_peer_owner_pubkeys();
let stored = manager.get_stored_user_record_json(peer_owner_pubkey_hex)?;
let authors = manager.get_message_push_author_pubkeys(peer_owner_pubkey_hex)?;
let session_states = manager.get_message_push_session_states(peer_owner_pubkey_hex)?;
```

- `known_peer_owner_pubkeys()` lists peer owner pubkeys known from loaded state or persisted
  storage, so callers can enumerate peers before `init()` without relying on filenames.
- `get_stored_user_record_json(peer)` returns the supported stored-user-record snapshot JSON for a
  peer owner, matching the library's persisted record shape without requiring callers to know
  filenames or storage layout.
- `get_message_push_author_pubkeys(peer)` returns the deduplicated sender pubkeys tracked by that
  peer's stored pairwise sessions.
- `get_message_push_session_states(peer)` returns session-state JSON snapshots plus tracked sender
  pubkeys and receiving-capability flags for push-routing repair flows.

### SessionManager App Loop

Mobile integrations should usually treat `SessionManagerHandle` as the main surface, not bare
`SessionHandle`. This is the path used by
[`iris-chat-flutter`](https://git.iris.to/#/npub1xdhnr9mrv47kkrn95k6cwecearydeh8e895990n3acntwvmgk2dsdeeycm/iris-chat-flutter).

```rust
let manager = SessionManagerHandle::new_with_storage_path(
    our_pubkey_hex,
    our_identity_privkey_hex,
    device_id,
    storage_path,
    owner_pubkey_hex,
)?;
manager.init()?;
manager.setup_user(peer_owner_pubkey_hex)?;

for event in manager.drain_events()? {
    match event.kind.as_str() {
        "publish" | "publish_signed" => {
            // Publish event.event_json to relays in the host app.
        }
        "subscribe" => {
            // Open subscription event.subid + event.filter_json in the host app.
        }
        "unsubscribe" => {
            // Close the matching subscription in the host app.
        }
        "decrypted_message" => {
            // Deliver plaintext to the app.
        }
        _ => {}
    }
}

// Feed relay events back into SessionManagerHandle.
manager.process_event(event_json)?;
```

Native consumers use this shape: app-owned relay I/O, drain pubsub events after `init`,
`setup_user`, send, and accept-invite calls, then feed relay events back with `process_event(...)`.

## Building for Mobile

### Android

```bash
# Prerequisites
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
cargo install cargo-ndk

# Build
./scripts/mobile/build-android.sh --release
```

Output:
- `target/android/jniLibs/{arm64-v8a,armeabi-v7a,x86_64,x86}/libndr_ffi.so`
- `target/android/bindings/*.kt`

### iOS

```bash
# Prerequisites (macOS only)
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios

# Build
./scripts/mobile/build-ios.sh --release
```

Output:
- `target/ios/NdrFfi.xcframework`
- `target/ios/bindings/*.swift`

## Testing

```bash
cargo test -p ndr-ffi
```

## Error Handling

All errors are mapped to `NdrError`:

- `InvalidKey` - Invalid key format
- `InvalidEvent` - Invalid event format
- `CryptoFailure` - Encryption/decryption error
- `StateMismatch` - Protocol state error
- `Serialization` - JSON serialization error
- `InviteError` - Invite-specific error
- `SessionNotReady` - Session cannot send yet

## License

MIT
