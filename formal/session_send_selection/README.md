# Session send selection model

This model captures the priority rule used when a device has multiple send-capable ratchet sessions
for the same peer device.

The checked rules match the TS and Rust session-manager implementations:

1. Prefer bidirectional sessions over send-only or receive-only sessions.
2. During outbound sends, keep an already active bidirectional session ahead of inactive
   bidirectional sessions, even when the inactive session has a higher receive count.
3. When directionality, active bonus, and receive count are tied, prefer the session from the newer
   ratchet epoch via `previousSendingChainMessageCount` before comparing the current sending count.

## Run TLC

```bash
./formal/session_send_selection/run_tlc.sh --mode all
./formal/session_send_selection/run_tlc.sh --mode ci
```

`--mode all` runs the two explanatory failing configs first, then the pass-expected configs.
`--mode ci` runs only the pass-expected configs.
