## Current State

This branch captures WIP related to the Flutter linked-device reopen bug.

Known result:
- `cargo test -p nostr-double-ratchet test_linked_receiver_restores_and_receives_after_restart -- --nocapture` passes

That means the current in-memory Rust restart regression is green. The remaining bug is still more likely in:
- Flutter app/provider restore logic
- file-backed persistence / FFI behavior
- or relay-backed replay handling on reopen

## Files Changed Here

- `rust/crates/nostr-double-ratchet/tests/session_manager_multi_device_test.rs`
- `ts/src/SessionManager.ts`
- `ts/tests/SessionManager.test.ts`

## What The WIP Does

- adds Rust restart coverage for a linked receiver being restored and receiving after restart
- updates TS `setupUser()` to fetch latest AppKeys and apply them immediately
- makes the TS send path await setup for both peer and self before determining fanout targets
- adds TS coverage for a linked sender's first reply reaching the peer's newly linked device

## Next Cut

1. If Flutter lower-level native/file-backed restart coverage fails, add the matching regression here.
2. Prefer reproducing with file-backed storage rather than only `InMemoryStorage`.
3. If Rust/TS core keeps passing but Flutter still fails, treat this repo as supporting evidence and keep the main fix in `iris-chat-flutter`.
