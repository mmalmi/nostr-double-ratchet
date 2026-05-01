# Changelog

## ndr 0.0.128 - 2026-05-01

- Mark the `ndr` CLI crate as deprecated. Use the `iris` CLI from the
  `iris-chat` crate instead.

## 0.0.127 - 2026-04-30

- Apply a verified owner's full AppKeys roster immediately when accepting invite-link and public-invite devices, including sibling public invite discovery.
- Preserve the owner-device session when AppKeys lists delegate devices only, so replies keep using the established peer session.
- Carry inner-message and target-device publish metadata through Rust, FFI, and `ndr send` so CLI interop returns the encrypted event for the intended device.

## 0.0.126 - 2026-04-30

- Route `ndr-ffi` session operations through `NdrRuntime` wrapper APIs so FFI consumers do not depend on direct `SessionManager` access.

## 0.0.125 - 2026-04-30

- Add `NdrRuntime::protocol_backfill_filters(...)` so host apps fetch NDR protocol catch-up through the runtime boundary instead of rebuilding AppKeys, device-invite, invite-response, and message-author filters themselves.
- Include current-device invite-response backfill in the runtime-owned filter set so linked devices can recover sibling sessions after offline periods.

## 0.0.124 - 2026-04-29

- Fix linked-device direct-message fanout by preferring the active bidirectional ratchet session over stale inactive receive snapshots while still falling back to send-capable inactive sessions when needed.
- Backfill recent pairwise session events before one-shot `ndr send` delivery so the CLI can catch up with relay-visible device/session state.
- Align TypeScript header decryption with Rust for same-chain events authored by the peer's advertised next ratchet key.

## 0.0.123 - 2026-04-29

- Publish the Rust crates with linked-secondary group delivery fixed for `ndr listen`.
- Publish the runtime device invite after link acceptance so peer devices can establish sessions to linked devices without re-enabling public invite discovery.

## 0.0.122 - 2026-04-29

- Publish the Rust crates with `ndr` command handling routed through `NdrRuntime`.
- Add runtime support APIs for CLI session, invite, group, listen, and send workflows without direct `SessionManager` access.
- Harden public invite publication, preserve required AppKeys devices in snapshots, and reduce CLI relay send latency.

## 0.0.121 - 2026-04-28

- Decrypt encrypted AppKeys device labels when owner-key runtimes backfill or subscribe to their own AppKeys events, preserving human device labels after relay refreshes.

## 0.0.120 - 2026-04-28

- Pass group inner event ids through `NdrRuntime` publish callbacks so apps can correlate one-to-many group publishes with local group message records.

## 0.0.119 - 2026-04-28

- Pass the inner rumor event id through `NdrRuntime` publish callbacks so apps can correlate encrypted outer publishes with local message records.

## 0.0.118 - 2026-04-28

- Reject direct-message wrapper events whose outer author is not the expected ratchet author for the session, preventing same-invite linked-device bootstraps from corrupting sibling sessions.
- Keep accepting peer invites until an active device session can actually receive, improving same-owner fanout when multiple runtimes register concurrently.
- Add runtime coverage for same-owner fanout across three registered runtimes.

## 0.0.117 - 2026-04-28

- Feed the runtime's current local owner AppKeys snapshot into the session core before owner setup, so same-owner multi-device sends can fan out to newly registered sibling devices even if the session AppKeys subscription missed an update.

## 0.0.116 - 2026-04-28

- Feed AppKeys events observed by `NdrRuntime` into the session core as well as runtime device state, keeping owner-device session discovery aligned with runtime AppKeys subscriptions.
- Feed locally published AppKeys registrations/revocations back into the session core before updating runtime state, reducing same-owner multi-device fanout races.

## 0.0.115 - 2026-04-28

- Preserve freshly accepted own-device sessions across delayed AppKeys snapshots that do not yet list the linked device, preventing duplicate linked-device ratchets.
- Extend linked-device fanout coverage for stale AppKeys arriving between link acceptance and device registration.

## 0.0.114 - 2026-04-28

- Deduplicate concurrent link-invite accepts so the TypeScript runtime cannot create parallel ratchet sessions for the same linked device.
- Add linked-device fanout coverage for duplicate link-accept races.

## 0.0.113 - 2026-04-28

- Subscribe newly added runtime direct-message authors immediately while keeping stale-author removal throttled, reducing missed live delivery during rapid ratchet author changes.
- Install accepted invite sessions before emitting the invite response publish, preventing duplicate invite acceptance from racing the local session state and diverging linked-device ratchets.
- Reuse existing device-invite sessions when replayed device invite events arrive after a session has already been accepted.
- Preserve newly accepted own-device sessions when an older AppKeys event lacking that device arrives before the confirming multi-device AppKeys event.
- Add runtime coverage for immediate author additions during the direct-message subscription throttle window.

## 0.0.112 - 2026-04-28

- Preserve queued runtime sends when provisional single-device setup is replaced by real AppKeys, so messages sent before peer discovery completes flush to the newly authorized devices.
- Add runtime coverage for async peer discovery plus linked-device fanout.

## 0.0.111 - 2026-04-28

- Flush queued device targets after `SessionManager.sendEvent(...)` so messages queued during session setup are delivered as soon as the just-created session becomes usable.

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
