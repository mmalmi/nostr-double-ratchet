# Direct message subscription model

This model captures the architectural split between `SessionManager` and runtime-owned direct
message relay subscriptions.

`SessionManager` owns session state and exposes the set of message-push author pubkeys to watch. It
must not emit or retain direct-message subscription state itself. `NdrRuntime` or another consumer
owns the relay subscription lifecycle by syncing its active subscription to the author set exposed by
`SessionManager`.

The model keeps cryptography abstract and checks the boundary that caused interoperability failures:

1. A session-state change can alter the author set that must be watched.
2. The runtime subscription is dirty until the runtime syncs against `SessionManager`.
3. When the runtime is clean, the subscription authors exactly match `SessionManager`.
4. Inbound relay events from tracked authors are eventually fed to the session manager once relays
   recover.
5. Removed authors are eventually unsubscribed instead of being kept in a stale runtime filter.

## Run TLC (developer mode)

```bash
./formal/direct_message_subscriptions/run_tlc.sh --mode all
```

`--mode all` runs:

- `DirectMessageSubscriptions.current.cfg` (expected to fail; demonstrates an integration that
  updates session state but never syncs the runtime direct-message subscription)
- `DirectMessageSubscriptions.fixed.cfg` (expected to satisfy subscription ownership and delivery)
- `DirectMessageSubscriptions.cleanup.pass.cfg` (expected to satisfy stale-author cleanup)

## Run TLC (CI mode)

```bash
./formal/direct_message_subscriptions/run_tlc.sh --mode ci
```

`--mode ci` runs only pass-expected configs and fails on any error.
