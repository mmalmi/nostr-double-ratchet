[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/mmalmi/nostr-double-ratchet)

# nostr-double-ratchet

End-to-end encrypted messaging primitives for Nostr, implemented in TypeScript and Rust.

Used by [chat.iris.to](https://chat.iris.to) and by CLI tooling in this repo.

## Install `ndr` CLI (latest release)

Linux/macOS one-liner (auto-detect arch + OS):

```bash
curl -fsSL "https://github.com/mmalmi/nostr-double-ratchet/releases/latest/download/ndr-$(uname -m | sed 's/arm64/aarch64/')-$(uname -s | tr '[:upper:]' '[:lower:]' | sed 's/darwin/apple-darwin/' | sed 's/linux/unknown-linux-musl/').tar.gz" | tar -xz && cd ndr && ./install.sh
```

For Windows/manual install options, see [Releases](https://github.com/mmalmi/nostr-double-ratchet/releases/latest).

## Status

- 1:1 messaging via Double Ratchet over Nostr events
- Multi-device identity model (owner key + device keys) with AppKeys
- Invite and link flows for session bootstrapping
- Group messaging with sender keys and one-to-many outer events
- Cross-language TS/Rust interoperability tests
- Breaking changes are still possible while APIs settle

## Security Guarantees And Properties

### Confidentiality

- 1:1 payloads are encrypted end-to-end (NIP-44 + Double Ratchet).
- Group payloads are encrypted with per-sender sender-key chains.
- Relays can still observe outer-event metadata (timing, pubkeys, kind), not plaintext.

### Forward Secrecy And Recovery

- Double Ratchet gives forward secrecy for 1:1 sessions.
- After compromise of a current chain key, future secrecy recovers after fresh ratchet steps (assuming attacker no longer controls endpoints).
- Group sender keys rotate and are redistributed to handle membership and key changes.

### Author And Device Verification

- Outer Nostr events are signature-verified.
- Session/identity attribution is bound to authenticated session context and owner/device mappings.
- `ownerPubkey` claims are verified against AppKeys for multi-device identities.
- Inner rumor `pubkey` is not trusted for sender identity decisions.
- Shared-channel group invite bootstrap requires signed inner payloads and owner/device consistency checks.

### Plausible Deniability

- Inner rumors are unsigned payloads transported inside encrypted channels.
- Recipients can verify a message came through an established secure session, but there is no strong non-repudiation proof for inner message authorship.

### Not Guaranteed

- No protection against a compromised endpoint/device.
- No global availability guarantee; delivery depends on relay reachability.
- No perfect metadata privacy (Nostr relays still see network-level and outer-event metadata).

## Group Messaging Model

Groups use a hybrid model:

1. Group metadata and sender-key distributions are sent over authenticated 1:1 ratchet sessions.
2. Each sending device has a per-group sender-event keypair and sender-key chain.
3. A group message is published once as a one-to-many encrypted outer event.
4. Members decrypt using sender-key state learned from pairwise distributions.
5. Shared-channel events are used only for signed bootstrap invites when members do not yet have a direct 1:1 session.

## Scalability And Tradeoffs

- Per-group message publish is O(1) (single outer event), which scales better than per-member ciphertext fanout.
- Sender-key distribution and metadata updates are O(number of members/devices), so membership churn is more expensive than steady-state messaging.
- Multi-device support improves usability but increases session count, subscriptions, and state management complexity.
- Delivery is eventually consistent; implementations use persistent queues/retries but cannot force relay delivery.

## Implementations

| Language | Directory | Package |
|----------|-----------|---------|
| TypeScript | [ts/](./ts) | [npm](https://www.npmjs.com/package/nostr-double-ratchet) |
| Rust | [rust/](./rust) | [crates.io](https://crates.io/crates/nostr-double-ratchet) |

## Repository Layout

- `ts/`: TypeScript library
- `rust/crates/nostr-double-ratchet/`: Rust core library
- `rust/crates/ndr/`: CLI built on the Rust library

## Development And Tests

```bash
# TypeScript tests
pnpm -C ts test:once

# Rust library tests
cargo test -p nostr-double-ratchet --manifest-path rust/Cargo.toml

# ndr (CLI + e2e + cross-language)
cargo test -p ndr --manifest-path rust/Cargo.toml
```

For language-specific usage, see:

- [ts/README.md](./ts/README.md)
- [rust/README.md](./rust/README.md)
