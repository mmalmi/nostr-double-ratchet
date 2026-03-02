# SessionManager fanout + revocation + relay model

This TLA+ model captures the queueing path relevant to eventual multi-device fanout and
device revocation cleanup, plus an abstract relay transport:

1. `Send` puts a message in discovery when devices are unknown.
2. `AppKeysDiscover` reveals known authorized devices.
3. `ExpandDiscovery` moves discovery entries to per-device queue entries.
4. `AppKeysRevoke` models replacement AppKeys that revoke devices.
5. `CleanupRevokedDevice` purges queue/session state for revoked devices.
6. `EstablishSession` enables per-device flushing.
7. `Flush` enqueues transport attempts to relay.
8. Relay actions model transport behavior:
   - `RelayDrop` (loss),
   - `RelayDelay` (delay),
   - `RelayDeliver` with nondeterministic target choice (reordering),
   - `RelayDuplicate` (duplication),
   - `RelayPartition` / `RelayRecover` (connectivity changes).

`SpecUnderRecovery` makes the recovery assumption explicit for liveness checks:

- `<>[] relayUp` (eventually, relay stays reachable).

To keep TLC state space finite/deterministic, transport duplication is bounded by
`MaxRelayCopies` in each cfg.

The key switch is `RemoveDiscoveryOnPartialExpansion`:

- `TRUE`: matches current behavior where discovery is removed even if some per-device enqueue fails.
- `FALSE`: keeps discovery until expansion succeeds for all currently known devices.

## Run TLC (developer mode)

```bash
./formal/session_manager_fanout/run_tlc.sh --mode all
```

`--mode all` runs:

- `SessionManagerFanout.current.cfg` (expected to find a counterexample)
- `SessionManagerFanout.fixed.cfg` (expected to satisfy fanout recovery under `SpecUnderRecovery`)
- `SessionManagerFanout.revocation.pass.cfg` (expected to satisfy revocation safety/liveness)
- `SessionManagerFanout.recovery.pass.cfg` (expected to satisfy recovery + revocation properties under `SpecUnderRecovery`)

## Run TLC (CI mode)

```bash
./formal/session_manager_fanout/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
