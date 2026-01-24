[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/mmalmi/nostr-double-ratchet)

# nostr-double-ratchet

Double ratchet encryption for Nostr with implementations in TypeScript and Rust.

**Used by [chat.iris.to](https://chat.iris.to)** - a secure messaging app built on Nostr.

- [x] 1-on-1 channel
- [ ] Group channel
- Invites for securely exchanging session keys
- Breaking changes are likely

## Implementations

| Language | Directory | Package |
|----------|-----------|---------|
| TypeScript | [ts/](./ts) | [npm](https://www.npmjs.com/package/nostr-double-ratchet) |
| Rust | [rust/](./rust) | [crates.io](https://crates.io/crates/nostr-double-ratchet) (soon) |

## TypeScript

```bash
cd ts
yarn install
yarn test
```

See [ts/README.md](./ts/README.md) for usage.

## Rust

```bash
cd rust
cargo test
```

See [rust/README.md](./rust/README.md) for usage.

## Documentation

- [TypeScript docs](https://mmalmi.github.io/nostr-double-ratchet/)
- [Source code](https://github.com/mmalmi/nostr-double-ratchet)
