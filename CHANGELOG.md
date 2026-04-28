# Changelog

## 0.0.110 - 2026-04-28

- Wait for relay-visible AppKeys when registering a linked device identity, matching current-device registration behavior and avoiding sends before linked-device fanout is established.

## 0.0.109 - 2026-04-28

- Emit decrypted TypeScript session messages through the same runtime event queue as publish/subscribe work, matching the Rust feed/drain model more closely.
- Keep `NdrRuntime.onSessionEvent` as the app-facing callback while routing it from drained runtime events instead of attaching directly to `SessionManager`.
- Add linked-device runtime coverage for owner messages delivered to a linked runtime after link invite registration.

## 0.0.108 - 2026-04-28

- Move TypeScript `NdrRuntime` to the Rust-style feed/emit split for session-manager relay work.
- Add runtime construction for `SessionManager` so AppKeys, invite, invite-response, and publish events are emitted by the session core and executed by `NdrRuntime`.

## 0.0.107 - 2026-04-28

- Expose session user records and message push author lookup through `NdrRuntime` so apps can treat the runtime as the production boundary.

## 0.0.106 - 2026-04-28

- Preserve relay AppKeys event timestamps when refreshing runtime device state, preventing linked devices from treating fresh registrations as stale.
- Forward the outer wrapper event through the SessionManager user-record bridge so `outerEventId` metadata is populated.

## 0.0.105 - 2026-04-28

- Add a notification-preview helper for decrypting candidate session events without mutating durable ratchet state.
- Include the outer wrapper event id in session event metadata so apps can cache notification previews by outer event id.

## 0.0.104 - 2026-04-28

- Publish a clean release after yanking/deprecating 0.0.103.
- Keep encrypted kind 1060 wrappers free of outer recipient routing tags.

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
