# Invite handshake model

This TLA+ model captures invite-response processing and owner-claim routing safety:

1. Response delivery/replay over an abstract relay (`Emit`, `RelayDeliver`, `RelayDrop`, `RelayDuplicate`).
2. Response processing with replay tracking (`processed`, `replayed`).
3. Owner-claim authorization policy:
   - AppKeys-authorized device claims when AppKeys are known.
   - Cached authorization or single-device fallback when AppKeys are unavailable.
4. Session creation from accepted responses.

`SpecUnderRecovery` adds the liveness assumption:

- `<>[] relayUp` (eventually, relay stays reachable).

## Run TLC (developer mode)

```bash
./formal/invite_handshake/run_tlc.sh --mode all
```

`--mode all` runs:

- `InviteHandshake.current.cfg` (expected to fail; demonstrates unauthorized-claim + replay-accept bugs)
- `InviteHandshake.fixed.cfg` (expected to satisfy safety + acceptance liveness)
- `InviteHandshake.recovery.pass.cfg` (expected to satisfy safety + liveness with single-device fallback)

## Run TLC (CI mode)

```bash
./formal/invite_handshake/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
