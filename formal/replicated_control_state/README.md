# Replicated control-state convergence model

This TLA+ model captures the deterministic merge rule used for owner-level chat control state
and group metadata state:

1. Every control mutation gets a totally ordered stamp.
2. Devices may receive mutations out of order.
3. Devices may replay stale mutations after they have already seen newer ones.
4. Delete mutations act as tombstones and must prevent stale resurrection.

The model does **not** prove the full messaging protocol. It proves the smaller thing we need for
multi-device convergence: once devices have seen the same control updates, stale replay cannot
make them diverge again.

## Run TLC (developer mode)

```bash
./formal/replicated_control_state/run_tlc.sh --mode all
```

`--mode all` runs:

- `ReplicatedControlState.current.cfg` (expected to fail; stale replay can resurrect older state)
- `ReplicatedControlState.fixed.cfg` (expected to satisfy the convergence invariants)

## Run TLC (CI mode)

```bash
./formal/replicated_control_state/run_tlc.sh --mode ci
```
