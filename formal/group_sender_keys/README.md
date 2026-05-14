# Group sender-key model

This TLA+ model captures group membership/admin control, sender-key distribution,
recipient-scoped sender-key repair snapshots, and relay behavior under failures.

Modeled dimensions:

1. Membership/admin transitions (`AddMember`, `RemoveMember`, `AddAdmin`, `RemoveAdmin`).
2. Sender-key key ids and per-key message iterations.
3. Per-recipient distribution repair snapshots. A repair response is only valid
   when the requester was an intended recipient of the distribution snapshot for
   the requested key id and message number.
4. Local sibling sync at owner abstraction: when enabled, the sender owner is
   also an intended recipient so linked devices can repair a missed local
   sibling sender-key distribution.
5. Distribution and message obligations (`needDist`, `needMsg`) plus pending
   blocked messages and repair requests.
6. Relay transport semantics:
   - `RelayDrop` (loss)
   - `RelayDelay` (delay)
   - `RelayDeliver*` (delivery/reordering)
   - `RelayDuplicate` (duplicate/idempotent under set semantics)
   - `RelayPartition` / `RelayRecover` (connectivity)
7. Revocation cleanup (`CleanupRemoved`) that purges stale transport/state.

`SpecUnderRecovery` adds explicit liveness assumption:

- `<>[] relayUp` (eventually relay connectivity remains up).

## Run TLC (developer mode)

```bash
./formal/group_sender_keys/run_tlc.sh --mode all
```

`--mode all` runs:

- `GroupSenderKeys.current.cfg` (expected to fail; demonstrates unauthorized-membership mutation bug)
- `GroupSenderKeys.repair-leak.current.cfg` (expected to fail; demonstrates key-history repair leaking pre-join key material)
- `GroupSenderKeys.fixed.cfg` (expected to satisfy safety invariants)
- `GroupSenderKeys.recovery.pass.cfg` (expected to satisfy safety + recovery-conditioned liveness)

`GroupSenderKeys.fixed.cfg` enables local sibling sync and checks that the sender owner receives
a recipient-scoped repair snapshot for local sibling recovery. `GroupSenderKeys.recovery.pass.cfg`
keeps local sibling sync disabled so temporal recovery checking stays small enough for routine runs;
the local sibling recovery path is covered by the fixed safety model plus the Rust regression test.

## Run TLC (CI mode)

```bash
./formal/group_sender_keys/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
