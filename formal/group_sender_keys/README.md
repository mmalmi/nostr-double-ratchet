# Group sender-key model

This TLA+ model captures group membership/admin control, sender-key distribution,
recipient-scoped sender-key repair snapshots, and relay behavior under failures.

Modeled dimensions:

1. Membership/admin transitions (`AddMember`, `RemoveMember`, `AddAdmin`, `RemoveAdmin`).
2. Sender-key key ids and per-key message iterations.
3. Per-recipient distribution repair snapshots. A repair response is only valid
   when the requester was an intended recipient of the distribution snapshot for
   the requested key id and message number.
4. Distribution and message obligations (`needDist`, `needMsg`) plus pending
   blocked messages and repair requests.
5. Relay transport semantics:
   - `RelayDrop` (loss)
   - `RelayDelay` (delay)
   - `RelayDeliver*` (delivery/reordering)
   - `RelayDuplicate` (duplicate/idempotent under set semantics)
   - `RelayPartition` / `RelayRecover` (connectivity)
6. Revocation cleanup (`CleanupRemoved`) that purges stale transport/state.

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

## Run TLC (CI mode)

```bash
./formal/group_sender_keys/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
