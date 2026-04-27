# Changelog

## 0.0.102 - 2026-04-27

- Pin the Rust crates to `nostr` 0.44.2 and `nostr-sdk` 0.44.1.
- Speed up `e2e_group_listen` by bounding concurrent real relay/listener scenarios instead of blocking Tokio worker threads with a synchronous global test lock.

## 0.0.101 - 2026-04-27

- Update the Rust crates to `nostr` 0.44 and `nostr-sdk` 0.44.
- Adapt CLI relay fetch, subscribe, and publish paths to the current single-filter and borrowed-event APIs.
- Preserve shared-channel self `p` tags under the newer Nostr event builder behavior.
- Refresh Rust, FFI, CLI, and interop tests for the new SDK APIs.

## 0.0.100 - 2026-04-26

- Align the Rust workspace/crate versions with the TypeScript package release.
- Update TypeScript e2e harnesses to use the current `receiveEvent(...)` and
  `Invite.accept(...)` APIs.
- Mirror the runtime-owned direct-message subscription architecture in the TypeScript package.
- Add fed-event `Session.receiveEvent(...)` and `SessionManager.processReceivedEvent(...)` paths for external relay dispatch.
- Have `NdrRuntime` subscribe to current direct-message authors and resubscribe when ratchet/session state changes.
- Add TypeScript runtime coverage for direct messages delivered through `NdrRuntime`.

## 0.0.99 - 2026-04-26

- Move direct-message subscription ownership from `Session`/`SessionManager` into `NdrRuntime`.
- Keep `Session` focused on ratchet state and encryption/decryption; callers now feed received events through runtime/manager APIs.
- Speed up `ndr listen` startup by removing the redundant filesystem watcher path and flushing runtime subscriptions immediately.
- Fix shared-channel group invite acceptance so accepted sessions are imported into `SessionManager` before follow-up messages.
- Tighten group-listener e2e coverage so a single group send must reach every other participant.

## 0.0.98 - 2026-04-26

- Preserve original inner rumor IDs when queued session-manager publishes flush later.
- Surface queued publish metadata through Rust and FFI event streams so apps can attach relay ACKs to the original queued chat message.
- Add regression coverage for queued publish metadata preservation across setup and retry flows.
