# nostr-double-ratchet-cli Plan

## Overview

CLI tool for encrypted Nostr messaging using double ratchet protocol. Designed for:
- AI agents (clawdbot, claude-code)
- Humans
- Automation/scripts

## Project Structure

```
rust/crates/
└── nostr-double-ratchet-cli/
    ├── Cargo.toml
    └── src/
        ├── main.rs           # CLI entry point (clap)
        ├── lib.rs            # Library exports for programmatic use
        ├── commands/         # Command implementations
        │   ├── mod.rs
        │   ├── identity.rs   # login, logout, whoami
        │   ├── invite.rs     # create, list, delete, listen
        │   ├── chat.rs       # list, join, delete, show
        │   └── message.rs    # send, read, listen
        ├── config.rs         # Config file (~/.nostr-double-ratchet/)
        ├── storage.rs        # SQLite storage for sessions/messages
        └── output.rs         # JSON/human output formatting
```

## Commands

### Identity
```bash
ndr login <nsec|hex>              # Login with private key
ndr logout                        # Clear identity and all data
ndr whoami                        # Show current pubkey
```

### Invites
```bash
ndr invite create [--label NAME]  # Create invite, output URL
ndr invite list                   # List all invites with usage
ndr invite delete <id>            # Delete invite
ndr invite listen                 # Listen for invite acceptances
```

### Chats
```bash
ndr chat list                     # List all chats
ndr chat join <url>               # Accept invite, start chat
ndr chat delete <id>              # Delete chat and messages
ndr chat show <id>                # Show chat details
```

### Messages
```bash
ndr send <chat-id> <message>      # Send encrypted message
ndr read <chat-id> [--limit N]    # Read messages
ndr listen [--chat <id>]          # Listen for new messages (all or specific)
```

## Agent-Friendly Features

### JSON Output Mode
All commands support `--json` / `-j` flag:
```bash
ndr --json chat list
```

Output format (matches clawdbot pattern):
```json
{
  "status": "ok",
  "command": "chat.list",
  "data": { "chats": [...] }
}
```

Error format:
```json
{
  "status": "error",
  "command": "chat.list",
  "error": "Not logged in"
}
```

### Non-Interactive
- No prompts by default
- All input via arguments
- Exit codes: 0=success, 1=error

### Streaming Output
For `listen` commands, output newline-delimited JSON:
```jsonl
{"event": "message", "chat": "abc123", "from": "npub...", "content": "Hello"}
{"event": "message", "chat": "abc123", "from": "npub...", "content": "Hi!"}
```

## Storage

Location: `~/.nostr-double-ratchet/`
- `config.toml` - Configuration
- `identity.json` - Encrypted private key
- `data.db` - SQLite database (sessions, messages, invites)

## Dependencies

```toml
[dependencies]
nostr-double-ratchet = { path = "../.." }
clap = { version = "4.5", features = ["derive", "env"] }
tokio = { version = "1", features = ["full"] }
nostr-sdk = "0.37"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.32", features = ["bundled"] }
dirs = "5"
anyhow = "1"
thiserror = "1"
```

## Implementation Order (TDD)

### Phase 1: Core Infrastructure
1. [ ] Project setup (Cargo.toml, main.rs)
2. [ ] Config module with tests
3. [ ] Storage module with tests
4. [ ] Output formatting with tests

### Phase 2: Identity
5. [ ] `login` command
6. [ ] `logout` command
7. [ ] `whoami` command

### Phase 3: Invites
8. [ ] `invite create` command
9. [ ] `invite list` command
10. [ ] `invite delete` command

### Phase 4: Chats
11. [ ] `chat join` command
12. [ ] `chat list` command
13. [ ] `chat delete` command

### Phase 5: Messaging
14. [ ] `send` command
15. [ ] `read` command
16. [ ] `listen` command (streaming)

### Phase 6: Polish
17. [ ] Error handling improvements
18. [ ] Help text and examples
19. [ ] Integration tests
