# nostr-double-ratchet (TypeScript)

TypeScript implementation of end-to-end encrypted Nostr messaging using Double Ratchet, multi-device owner/device identity mapping, and sender-key-based group messaging.

## Installation

```bash
pnpm add nostr-double-ratchet
```

## Core Components

- `Session`: low-level 1:1 ratchet session
- `Invite`: handshake/bootstrap primitive
- `SessionManager`: multi-device session orchestration and routing
- `SessionGroupRuntime`: group transport bound to an existing `SessionManager`
- `DelegateManager` / `AppKeysManager`: device lifecycle and owner authorization
- `NdrRuntime`: high-level runtime that owns AppKeys, delegate/session state, and group transport
- `Group`: sender-key group messaging helper (transport-agnostic)
- `SharedChannel`: encrypted shared-channel primitive used by higher-level group bootstrap flows

## Choose Your Layer

- `Session`: use this when you already own invite/bootstrap, persistence, and relay transport.
- `SessionManager`: use this when you want multi-device routing but still want to own app runtime wiring.
- `SessionGroupRuntime`: use this when you already have a `SessionManager` and want the same group
  transport surface as `NdrRuntime` without moving AppKeys/delegate/session ownership.
- `NdrRuntime`: use this when you want the default high-level path for production apps. It owns
  `AppKeysManager`, `DelegateManager`, `SessionManager`, and `GroupManager`.

## Quick Start (`NdrRuntime`)

```typescript
import { NdrRuntime } from "nostr-double-ratchet";

const runtime = new NdrRuntime({
  nostrSubscribe,
  nostrPublish,
  nostrFetch,
  storage,
});

await runtime.initForOwner(ownerPublicKey);

runtime.onSessionEvent((event, from) => {
  console.log(`${from}: ${event.content}`);
});
runtime.onGroupEvent((event) => {
  console.log(`group ${event.groupId}: ${event.inner.content}`);
});

const sessionManager = await runtime.waitForSessionManager(ownerPublicKey);
const groupManager = await runtime.waitForGroupManager(ownerPublicKey);
await sessionManager.sendMessage(recipientPubkey, "Hello!");

const created = await runtime.createGroup("Friends", [recipientPubkey], {
  fanoutMetadata: false,
});
await runtime.sendGroupMessage(created.group.id, "Hello group!");
console.log(groupManager.managedGroupIds());
```

`NdrRuntime` does not own your relay client. It still relies on your `nostrSubscribe`,
`nostrFetch`, and `nostrPublish` functions.

### Runtime Group API

If you want the high-level path, keep groups on `NdrRuntime` instead of building a parallel
group transport layer:

- `getGroupManager()` / `waitForGroupManager(ownerPubkey?)`
- `onGroupEvent(...)`
- `upsertGroup(...)`, `removeGroup(...)`, `syncGroups(...)`
- `createGroup(...)`
- `sendGroupEvent(...)`, `sendGroupMessage(...)`

`GroupManager` is still available directly when you want to own more of the app wiring yourself.

## Mid-Level Setup (`SessionGroupRuntime`)

If your app already owns `SessionManager`, `SessionGroupRuntime` gives you the same group-focused
API shape that `NdrRuntime` uses internally:

```typescript
import { SessionGroupRuntime } from "nostr-double-ratchet";

const groups = new SessionGroupRuntime({
  sessionManager,
  ourOwnerPubkey: ownerPublicKey,
  ourDevicePubkey: currentDevicePublicKey,
  nostrSubscribe,
  nostrPublish,
  nostrFetch,
});

const created = await groups.createGroup("Friends", [recipientPubkey], {
  fanoutMetadata: false,
});
await groups.sendGroupMessage(created.group.id, "Hello group!");

groups.onGroupEvent((event) => {
  console.log(event.groupId, event.inner.content);
});
```

## Low-Level Setup (`SessionManager`)

If you want multi-device sessions without the runtime wrapper, the lower-level flow remains:

```typescript
import { AppKeysManager, DelegateManager } from "nostr-double-ratchet";

const delegate = new DelegateManager({ nostrSubscribe, nostrPublish, storage });
await delegate.init();

const appKeysManager = new AppKeysManager({ nostrPublish, storage });
await appKeysManager.init();
appKeysManager.addDevice(delegate.getRegistrationPayload());
await appKeysManager.publish();

await delegate.activate(ownerPublicKey);
const sessionManager = delegate.createSessionManager();
await sessionManager.init();
```

## Multi-Device Integration Contract

Use the exported helper functions for multi-device policy instead of duplicating the logic in app
code.

- `applyAppKeysSnapshot(...)`: order AppKeys by `created_at`, ignore stale snapshots, and merge
  same-second snapshots monotonically.
- `evaluateDeviceRegistrationState(...)`: decide whether the current device is registered and
  whether the app should consider private messaging ready.
- `shouldRequireRelayRegistrationConfirmation(...)`: distinguish first-device bootstrap from
  “add a new device to an existing owner timeline” before blocking on relay visibility.
- `resolveConversationCandidatePubkeys(...)`: derive the correct conversation owner/device
  candidates for self-sync and linked-device routing.
- `resolveInviteOwnerRouting(...)`: preserve inviter owner/device attribution during invite
  acceptance, including the link-bootstrap exception and device-identity fallback.
- `DirectMessageSubscriptionTracker` + `buildDirectMessageBackfillFilter(...)`: detect newly added
  direct-message session authors and issue a short replay/backfill right away.
- `resolveSessionPubkeyToOwner(...)` and `hasExistingSessionWithRecipient(...)`: normalize
  owner/device session bookkeeping instead of re-implementing user-record traversal.

Normal app behavior should treat AppKeys as an ordered authorization timeline. Reduced AppKeys
sets should only be published for explicit revocation or first-device bootstrap, not on ordinary
startup. Imported owner-key or `nsec` logins on a fresh device should either register the current
device or remain explicitly single-device. First-device bootstrap can proceed from locally
published AppKeys; public-invite fanout for an additional device should wait until relays echo the
updated AppKeys timeline.

## Direct Message Catch-Up

The runtime/session manager decides which session authors should be live-subscribed, but your app
still owns relay fetch/backfill. Wrap your `nostrSubscribe` implementation so newly added direct
message authors trigger a short replay immediately:

```typescript
import {
  buildDirectMessageBackfillFilter,
  DirectMessageSubscriptionTracker,
} from "nostr-double-ratchet";

const tracker = new DirectMessageSubscriptionTracker();

const trackedSubscribe = (filter, onEvent) => {
  const { token, addedAuthors } = tracker.registerFilter(filter);
  if (addedAuthors.length) {
    const backfill = buildDirectMessageBackfillFilter(
      addedAuthors,
      Math.floor(Date.now() / 1000) - 15,
      200,
    );
    // Hand `backfill` to your relay fetch / short-lived subscription path.
  }

  const unsubscribe = nostrSubscribe(filter, onEvent);
  return () => {
    tracker.unregister(token);
    unsubscribe();
  };
};
```

## Security Properties

### Confidentiality

- 1:1 payloads are encrypted with Double Ratchet over NIP-44.
- Group payloads are encrypted with per-sender sender-key chains.

### Forward Secrecy And Post-Compromise Recovery

- 1:1 sessions get forward secrecy from ratcheting key evolution.
- Future secrecy recovers after new ratchet steps if a transient compromise ends.

### Author And Device Verification

- Outer events are signature-verified.
- Sender owner/device attribution comes from authenticated session context and AppKeys mappings.
- Multi-device owner claims are verified against AppKeys (not accepted blindly).
- The latest AppKeys set is authoritative for device authorization; removing a device from AppKeys revokes it for future routing and owner-claim validation.
- Applications must not publish a reduced AppKeys set implicitly during startup/reopen. Publishing fewer devices should only happen for explicit device revocation or first-device bootstrap.
- Inner rumor `pubkey` should be treated as untrusted for identity decisions.

### Plausible Deniability

- Inner rumors are unsigned payloads carried in encrypted channels.
- You get channel authenticity, but not strong transferable non-repudiation of inner content.

## Groups

`Group` is transport-agnostic and implements efficient sender-key messaging:

- Membership is defined in owner pubkeys.
- Sender-key distributions are delivered pairwise over authenticated 1:1 sessions.
- Group messages are published once as one-to-many outer events (per sender device key).
- Receiver attribution for group payloads is derived from authenticated distribution/session context, not inner rumor `pubkey`.
- `createGroupData(...)` is a pure local constructor. `GroupManager.createGroup(...)` is the app-level helper that creates local group state and, by default, fans out metadata (kind 40) to members.
- `NdrRuntime` now exposes the same high-level group path as Rust/FFI: `createGroup(...)`,
  `sendGroupEvent(...)`, `sendGroupMessage(...)`, `syncGroups(...)`, and `onGroupEvent(...)`.

## Disappearing Messages (Expiration)

Include NIP-40-style `["expiration", "<unix seconds>"]` on the inner rumor, or use helpers:

```typescript
await sessionManager.sendMessage(recipientPubkey, "expires soon", {
  ttlSeconds: 60,
});
await sessionManager.setDefaultExpiration({ ttlSeconds: 60 });
await sessionManager.setExpirationForPeer(recipientPubkey, { ttlSeconds: 120 });
await sessionManager.setExpirationForGroup(groupId, { ttlSeconds: 30 });
await sessionManager.setExpirationForPeer(recipientPubkey, null);
await sessionManager.sendMessage(recipientPubkey, "persist", {
  expiration: null,
});
```

The library does not purge local storage for you. Clients should enforce retention/UI behavior.

## 1:1 Chat Settings Signaling

Encrypted settings rumor kind:

- `CHAT_SETTINGS_KIND = 10448`
- Content: `{ "type": "chat-settings", "v": 1, "messageTtlSeconds": <seconds|null> }`
- Settings events themselves do not expire

```ts
await sessionManager.setChatSettingsForPeer(recipientPubkey, 60);
await sessionManager.setChatSettingsForPeer(recipientPubkey, 0);
sessionManager.setAutoAdoptChatSettings(false);
```

## Event Kinds

| Kind  | Constant                                    | Purpose                                      |
| ----- | ------------------------------------------- | -------------------------------------------- |
| 1060  | `MESSAGE_EVENT_KIND`                        | Encrypted outer event                        |
| 30078 | `INVITE_EVENT_KIND` / `APP_KEYS_EVENT_KIND` | Device invite and AppKeys records            |
| 1059  | `INVITE_RESPONSE_KIND`                      | Encrypted invite response                    |
| 14    | `CHAT_MESSAGE_KIND`                         | Inner chat message rumor                     |
| 10448 | `CHAT_SETTINGS_KIND`                        | Inner chat-settings rumor                    |
| 40    | `GROUP_METADATA_KIND`                       | Group metadata rumor                         |
| 10445 | `GROUP_INVITE_RUMOR_KIND`                   | Group bootstrap invite rumor                 |
| 10446 | `GROUP_SENDER_KEY_DISTRIBUTION_KIND`        | Group sender-key distribution rumor          |
| 10447 | `GROUP_SENDER_KEY_MESSAGE_KIND`             | Group sender-key message rumor kind constant |
| 4     | `SHARED_CHANNEL_KIND`                       | Shared-channel transport event kind          |

## Scalability And Tradeoffs

- Group steady-state publish is O(1) per message (single outer event).
- Sender-key distribution and metadata fanout are O(members/devices), so membership changes are the expensive path.
- Multi-device support improves UX but increases session/subscription/state complexity.
- Relay-level metadata (timing, pubkeys, traffic patterns) remains visible.

## Development

```bash
pnpm -C ts test:once
```

## Multi-Device Test Policy

- Keep exactly one explicit same-second AppKeys regression test in the library.
- Ordinary end-to-end tests should wait for a new `created_at` second before publishing AppKeys
  mutations so the normal path stays visible and trustworthy.
- Keep heterogeneous-client interop coverage because same-client tests are not enough.
