# nostr-double-ratchet (Rust)

Rust implementation and tooling for Double Ratchet messaging on Nostr.

## Crates

| Crate | Description |
|-------|-------------|
| [nostr-double-ratchet](./crates/nostr-double-ratchet) | Core library: sessions, invites, session manager, groups |
| [ndr](./crates/ndr) | CLI built on the core library |
| [ndr-ffi](./crates/ndr-ffi) | UniFFI bindings for mobile integration |

## Security Properties (Rust Stack)

- End-to-end confidentiality for 1:1 messaging (NIP-44 + Double Ratchet).
- Forward secrecy and post-compromise recovery through ratchet evolution.
- Signed outer events with owner/device attribution bound to authenticated session context.
- Multi-device owner claims checked via AppKeys when owner and device keys differ.
- Inner rumor `pubkey` is not used as a trusted sender identity source.
- Inner rumors are unsigned, giving plausible deniability rather than non-repudiation.

## Group Model

- Group membership is represented by owner pubkeys.
- Group metadata and sender-key distributions are delivered over authenticated 1:1 sessions.
- Group messages are published once using per-sender one-to-many outer events.
- Shared-channel events are used for signed group bootstrap invites when needed to establish missing pairwise sessions.

## Quick Start

Commands below assume you run them from the repository root.

```bash
# Run all Rust tests in workspace
cargo test --manifest-path rust/Cargo.toml

# Install CLI
cargo install --path rust/crates/ndr

# Run CLI directly
cargo run -p ndr --manifest-path rust/Cargo.toml -- --help
```

## Important Test Targets

```bash
# Core library
cargo test -p nostr-double-ratchet --manifest-path rust/Cargo.toml

# CLI + e2e + interop
cargo test -p ndr --manifest-path rust/Cargo.toml

# Explicit cross-language suites
cargo test -p ndr --test e2e_crosslang --manifest-path rust/Cargo.toml
cargo test -p ndr --test e2e_group_crosslang --manifest-path rust/Cargo.toml
```

## Publishing

```bash
./rust/scripts/publish.sh --dry-run
./rust/scripts/publish.sh
```

For detailed core-library API and behavior, see [`rust/crates/nostr-double-ratchet/README.md`](./crates/nostr-double-ratchet/README.md).
