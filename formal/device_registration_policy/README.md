# Device registration policy model

This TLA+ model captures the policy distinction we now rely on for multi-device invite routing:

1. A first imported device may proceed from locally published AppKeys before relays reflect them.
2. An additional device on an existing owner timeline must not be trusted for public-invite routing
   until relays show the updated AppKeys snapshot.
3. Once relay visibility catches up, both scenarios should eventually become usable under relay
   recovery.

The model treats two scenarios in parallel:

- `bootstrap`: no previous devices were known
- `additional`: an existing owner timeline already exists

`RouteReady(s)` encodes the policy for when public-invite routing is allowed to trust the current
device in each scenario.

The failing configs demonstrate both sides of the policy tradeoff:

- `DeviceRegistrationPolicy.current.cfg`
  shows that always trusting locally published AppKeys is unsafe for additional devices.
- `DeviceRegistrationPolicy.bootstrap.current.cfg`
  shows that always requiring relay confirmation is too strict for first-device bootstrap.

The fixed configs prove the split policy:

- `DeviceRegistrationPolicy.fixed.cfg`
  satisfies both safety invariants.
- `DeviceRegistrationPolicy.recovery.pass.cfg`
  satisfies both safety invariants and eventual acceptance under `<>[] relayUp`.

The main learning from TLC is that there is no single global rule that works for both cases.
If we always trust local AppKeys, an additional device can be used before the rest of the system
can verify it. If we always require relay visibility, first-device bootstrap loses the local-only
path that makes imported-`nsec` recovery usable.

## Run TLC (developer mode)

```bash
./formal/device_registration_policy/run_tlc.sh --mode all
```

`--mode all` runs:

- `DeviceRegistrationPolicy.current.cfg` (expected to fail)
- `DeviceRegistrationPolicy.bootstrap.current.cfg` (expected to fail)
- `DeviceRegistrationPolicy.fixed.cfg` (expected to pass)
- `DeviceRegistrationPolicy.recovery.pass.cfg` (expected to pass)

## Run TLC (CI mode)

```bash
./formal/device_registration_policy/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
