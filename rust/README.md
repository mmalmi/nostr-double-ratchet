# nostr-double-ratchet (Rust)

Rust implementation of double ratchet encryption for Nostr.

Used by [chat.iris.to](https://chat.iris.to) for forward-secure messaging.

## Crates

| Crate | Description |
|-------|-------------|
| [nostr-double-ratchet](./crates/nostr-double-ratchet) | Core library implementing the double ratchet protocol |
| [ndr](./crates/ndr) | CLI for encrypted Nostr messaging |

## Quick Start

```bash
# Run tests
cargo test

# Install CLI
cargo install --path crates/ndr

# Or run directly
cargo run -p ndr -- --help
```

## Publishing

```bash
# Dry run
./scripts/publish.sh --dry-run

# Publish to crates.io
./scripts/publish.sh
```

## Cross-language Tests

E2E tests verify Rust and TypeScript implementations can communicate:

```bash
cargo test -p ndr --test e2e_crosslang
```
