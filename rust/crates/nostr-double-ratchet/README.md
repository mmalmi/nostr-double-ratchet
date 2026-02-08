# nostr-double-ratchet

Rust implementation of the Double Ratchet protocol for Nostr, providing forward-secure end-to-end encrypted messaging.

## Overview

Based on Signal's Double Ratchet with header encryption, this crate implements secure session management over Nostr events. Messages are encrypted using NIP-44 and the double ratchet algorithm ensures forward secrecy even if keys are compromised.

## Features

- **Double Ratchet encryption** - Forward secrecy with automatic key rotation
- **Out-of-order message handling** - Skipped message keys cached for delivery flexibility
- **Session persistence** - Serialize/deserialize session state
- **NIP-44 integration** - Uses Nostr's standardized encryption
- **Type-safe** - Leverages enostr's Pubkey type

## Usage

```rust
use nostr_double_ratchet::{Session, Result};
use nostr::Keys;

// Initialize sessions
let alice_keys = Keys::generate();
let bob_keys = Keys::generate();

let shared_secret = [0u8; 32]; // Exchange securely via invite mechanism

let mut alice = Session::init(
    bob_pubkey,
    alice_keys.secret_key().to_secret_bytes(),
    true,  // initiator
    shared_secret,
    Some("alice".to_string()),
)?;

let mut bob = Session::init(
    alice_pubkey,
    bob_keys.secret_key().to_secret_bytes(),
    false,  // responder
    shared_secret,
    Some("bob".to_string()),
)?;

// Send encrypted message
let event = alice.send("Hello Bob!".to_string())?;

// Receive and decrypt
let plaintext = bob.receive(&event)?;
```

## Disappearing Messages (Expiration)

For disappearing messages, include a NIP-40-style `["expiration", "<unix seconds>"]` tag in the *inner* rumor event.
When sending via `SessionManager`, you can set per-send overrides or configure defaults:

```rust
use nostr_double_ratchet::{SendOptions, SessionManager};

// Assuming you already have a SessionManager named `manager`.

// Apply a global default (e.g. 60s from now)
manager.set_default_send_options(Some(SendOptions {
    ttl_seconds: Some(60),
    expires_at: None,
}))?;

// Per-peer default (overrides global default)
manager.set_peer_send_options(bob_pubkey, Some(SendOptions {
    ttl_seconds: Some(120),
    expires_at: None,
}))?;

// Use defaults (no per-send options)
manager.send_text(bob_pubkey, "hi".to_string(), None)?;

// Disable expiration for a single send even if defaults exist
manager.send_text(bob_pubkey, "persist".to_string(), Some(SendOptions::default()))?;
```

## Tests

Run the test suite:

```bash
cargo test -p nostr-double-ratchet
```

Tests cover:
- Session initialization (initiator/responder)
- Message encryption/decryption
- Multi-message conversations
- Out-of-order delivery
- Session persistence
- Consecutive messages with ratchet stepping

## Architecture

- `Session` - Core double ratchet implementation
- `SessionState` - Serializable session state with all keys and counters
- `Header` - Message metadata (sequence number, next public key, chain length)
- Utilities - KDF using HKDF-SHA256, serialization helpers

## Status

Core functionality complete, including SessionManager, Invite, and multi-device support via AppKeys.
