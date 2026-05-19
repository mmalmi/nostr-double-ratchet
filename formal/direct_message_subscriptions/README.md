# Direct message subscription model

This model captures the current AppCore-owned direct message subscription plan.

`SessionManager` owns session state and exposes the set of message-push author pubkeys to watch. It
must not emit or retain direct-message relay subscription state itself. AppCore derives one desired
protocol plan from protocol state, applies it with one bounded relay subscription operation, and only
records the plan as applied after the relay operation succeeds.

The model keeps cryptography abstract and checks the boundary that caused interoperability failures:

1. A session-state change can alter the author set that must be watched.
2. Skipped-key sender pubkeys are part of the author set, because out-of-order old messages may
   still arrive from those ephemeral keys.
3. Newly added authors enter `desiredPlan` immediately. The live relay client may lag while the
   bounded apply is in flight, but the desired plan remains honest.
4. AppCore tracks `desiredPlan`, `applyingPlan`, and `appliedPlan` separately. `appliedPlan` is only
   advanced after successful relay subscription apply.
5. Failed or timed-out subscription apply always clears the in-flight flag and preserves a dirty
   refresh for retry.
6. Catch-up/backfill is derived from `desiredPlan`, not from the live applied relay subscription, so
   a stuck or failed apply cannot permanently suppress recovery.
7. When AppCore is clean, the applied plan exactly matches the desired protocol state.
8. Removed authors are eventually unsubscribed instead of being kept in a stale live filter.

## Run TLC (developer mode)

```bash
./formal/direct_message_subscriptions/run_tlc.sh --mode all
```

`--mode all` runs:

- `DirectMessageSubscriptions.current.cfg` (expected to fail; demonstrates an integration that
  updates session state but never refreshes the desired AppCore protocol plan)
- `DirectMessageSubscriptions.added-author.current.cfg` (expected to fail; demonstrates throttling
  a newly added author out of the desired plan)
- `DirectMessageSubscriptions.skipped-author.current.cfg` (expected to fail; demonstrates omitting
  skipped-key sender pubkeys from the watched author set)
- `DirectMessageSubscriptions.applied-before-success.current.cfg` (expected to fail; demonstrates
  reporting a desired plan as applied before relay subscribe succeeds)
- `DirectMessageSubscriptions.stuck-apply.current.cfg` (expected to fail; demonstrates an apply flag
  that never clears and catch-up derived from applied state)
- `DirectMessageSubscriptions.fixed.cfg` (expected to satisfy subscription ownership and delivery)
- `DirectMessageSubscriptions.cleanup.pass.cfg` (expected to satisfy stale-author cleanup)

## Run TLC (CI mode)

```bash
./formal/direct_message_subscriptions/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
