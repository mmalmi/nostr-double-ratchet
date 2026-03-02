# SessionManager fanout model

This TLA+ model captures the queueing path relevant to eventual multi-device fanout:

1. `Send` puts a message in discovery when devices are unknown.
2. `AppKeysUpdate` reveals known devices.
3. `ExpandDiscovery` moves discovery entries to per-device queue entries.
4. `EstablishSession` enables per-device flushing.
5. `Flush` publishes and marks per-device delivery.

The key switch is `RemoveDiscoveryOnPartialExpansion`:

- `TRUE`: matches current behavior where discovery is removed even if some per-device enqueue fails.
- `FALSE`: keeps discovery until expansion succeeds for all currently known devices.

## Run TLC

```bash
./formal/session_manager_fanout/run_tlc.sh
```

The script runs:

- `SessionManagerFanout.current.cfg` (expected to find a counterexample)
- `SessionManagerFanout.fixed.cfg` (expected to satisfy checked properties)
