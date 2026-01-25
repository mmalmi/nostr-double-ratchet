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
