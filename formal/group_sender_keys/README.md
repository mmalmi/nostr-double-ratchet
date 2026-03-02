# Group sender-key model

This TLA+ model captures group membership/admin control, sender-key epoch distribution,
and relay behavior under failures.

Modeled dimensions:

1. Membership/admin transitions (`AddMember`, `RemoveMember`, `AddAdmin`, `RemoveAdmin`).
2. Sender-key epochs (`epoch`) and rotation.
3. Distribution and message obligations (`needDist`, `needMsg`).
4. Relay transport semantics:
   - `RelayDrop` (loss)
   - `RelayDelay` (delay)
   - `RelayDeliver*` (delivery/reordering)
   - `RelayDuplicate` (duplicate/idempotent under set semantics)
   - `RelayPartition` / `RelayRecover` (connectivity)
5. Revocation cleanup (`CleanupRemoved`) that purges stale transport/state.

`SpecUnderRecovery` adds explicit liveness assumption:

- `<>[] relayUp` (eventually relay connectivity remains up).

## Run TLC (developer mode)

```bash
./formal/group_sender_keys/run_tlc.sh --mode all
```

`--mode all` runs:

- `GroupSenderKeys.current.cfg` (expected to fail; demonstrates unauthorized-membership mutation bug)
- `GroupSenderKeys.fixed.cfg` (expected to satisfy safety invariants)
- `GroupSenderKeys.recovery.pass.cfg` (expected to satisfy safety + recovery-conditioned liveness)

## Run TLC (CI mode)

```bash
./formal/group_sender_keys/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
