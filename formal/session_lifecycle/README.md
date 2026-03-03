# Session lifecycle model

This TLA+ model captures per-device session lifecycle and delivery guarantees:

1. Session establishment/rotation/promotion (`EstablishSession`, `RotateSession`, `PromoteInactive`).
2. Single-active-session and bounded-inactive-session constraints.
3. Send/flush/deliver pipeline over an abstract relay (`QueueSend`, `Flush`, relay actions).
4. AppKeys replacement revocation and cleanup (`AppKeysRevoke`, `CleanupRevoked`).

`SpecUnderRecovery` adds the liveness assumption:

- `<>[] relayUp` (eventually, relay stays reachable).

## Run TLC (developer mode)

```bash
./formal/session_lifecycle/run_tlc.sh --mode all
```

`--mode all` runs:

- `SessionLifecycle.current.cfg` (expected to fail; demonstrates multiple-active-session + revoked-delivery bugs)
- `SessionLifecycle.fixed.cfg` (expected to satisfy session safety + delivery liveness)
- `SessionLifecycle.recovery.pass.cfg` (expected to satisfy session + revocation safety/liveness)

## Run TLC (CI mode)

```bash
./formal/session_lifecycle/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
