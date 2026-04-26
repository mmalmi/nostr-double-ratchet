# Formal Models

This directory contains focused TLA+ models for protocol rules that have been easy to get wrong in
practice.

## Current Models

- [`session_manager_fanout`](./session_manager_fanout):
  AppKeys ordering, stale replay rejection, same-second monotonic merge behavior, revocation, and
  eventual fanout recovery.
- [`invite_handshake`](./invite_handshake):
  invite replay handling, unauthorized owner-claim rejection, and single-device fallback during
  bootstrap.
- [`device_registration_policy`](./device_registration_policy):
  the split registration policy for imported devices:
  first-device bootstrap may proceed from locally published AppKeys, but adding a new device to an
  existing owner timeline should wait for relay-visible AppKeys before public-invite routing trusts
  it.
- [`session_lifecycle`](./session_lifecycle):
  session progression and recovery properties.
- [`replicated_control_state`](./replicated_control_state):
  replicated invite/control state convergence.
- [`group_sender_keys`](./group_sender_keys):
  sender-key distribution and recovery for groups.
- [`direct_message_subscriptions`](./direct_message_subscriptions):
  runtime-owned direct-message subscription synchronization from `SessionManager` message-push
  author state.

## Main Lessons So Far

- AppKeys are an authorization timeline, not just a set. Stale snapshots must not override newer
  state, and same-second replays must merge monotonically.
- Invite acceptance and owner attribution need explicit safety checks; replay resistance alone is
  not enough.
- Device registration needs a split policy. TLC shows there is no single global rule that safely
  covers both bootstrap and additional-device flows:
  trusting locally published AppKeys is unsafe for additional devices, while requiring relay
  visibility everywhere breaks first-device bootstrap and recovery.
- Multi-relay behavior matters at the policy boundary, but only in the abstract sense of
  `locally published` versus `relay-visible`. Silent or lagging relays are worth testing end to
  end, while TLA+ should focus on whether enough relay-visible state exists for other participants
  to verify an authorization change.
- Direct-message subscriptions are runtime/consumer state. `SessionManager` should expose the
  author pubkeys that can carry ratchet messages, and the runtime must sync its relay subscription
  whenever that author set changes. Otherwise valid ratchet state can still miss inbound messages.

## Running Models

Each model directory includes its own `README.md` and `run_tlc.sh`.

Typical usage:

```bash
./formal/device_registration_policy/run_tlc.sh --mode all
./formal/session_manager_fanout/run_tlc.sh --mode ci
```
