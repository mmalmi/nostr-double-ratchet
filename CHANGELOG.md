# Changelog

## 0.0.98 - 2026-04-26

- Preserve original inner rumor IDs when queued session-manager publishes flush later.
- Surface queued publish metadata through Rust and FFI event streams so apps can attach relay ACKs to the original queued chat message.
- Add regression coverage for queued publish metadata preservation across setup and retry flows.
