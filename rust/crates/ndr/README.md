# ndr

CLI for encrypted Nostr messaging using the double ratchet protocol.

Designed for humans, AI agents, and automation. Compatible with [chat.iris.to](https://chat.iris.to).

## Installation

```bash
cargo install ndr
```

Or build from source:

```bash
cargo install --path .
```

## Quick Start

```bash
# Login with a private key (hex or nsec)
ndr login <private_key>

# Check identity
ndr whoami

# Create an invite
ndr invite create

# Publish a device invite (default device id = your identity pubkey hex)
ndr invite publish

# Join someone's invite
ndr chat join <invite_url>

# Send a message
ndr send <chat_id> "Hello!"

# Read messages
ndr read <chat_id>

# Listen for new messages in real-time
ndr listen
```

## JSON Mode

Use `--json` flag for machine-readable output (for scripts and AI agents):

```bash
ndr --json whoami
ndr --json chat list
ndr --json send abc123 "Hello from automation"
```

## Commands

### Identity

```bash
ndr login <key>     # Login with private key (hex or nsec)
ndr logout          # Logout and clear data
ndr whoami          # Show current identity
```

### Invites

```bash
ndr invite create           # Create new invite URL
ndr invite publish          # Create + publish invite event on Nostr (default device id: your pubkey hex)
ndr invite list             # List pending invites
ndr invite delete <id>      # Delete an invite
ndr invite listen           # Listen for invite acceptances
```

Notes:
- `ndr invite publish` defaults to device id = your identity pubkey (hex), matching Iris multi-device expectations.
- Re-run `ndr invite publish` to refresh the device invite.
- If you want a single "public" invite per identity across devices, use `--device-id public`.

### Link Devices

```bash
# On the device to be linked
ndr link create             # Create a private link invite (stores it locally)

# On the owner device
ndr link accept <url>       # Accept the link invite, publish response, and register device in AppKeys

# Back on the device to be linked
ndr listen                  # Wait for the response and finalize linking
```

### Chats

```bash
ndr chat list               # List all chats
ndr chat join <url>         # Join via invite URL, or Iris-style chat link: https://chat.iris.to/#npub1...
ndr chat show <id>          # Show chat details
ndr chat delete <id>        # Delete a chat
```

### Messages

```bash
ndr send <chat_id> <msg>    # Send encrypted message
ndr send <npub> <msg>       # Auto-accept their public invite if no chat exists
ndr send <chat_link> <msg>  # Same, but with https://chat.iris.to/#npub1... style links
ndr react <chat_id> <msg_id> <emoji>  # React to a message
ndr read <chat_id>          # Read message history
ndr listen                  # Listen for incoming messages
ndr listen --chat <id>      # Listen on specific chat
ndr receive <event_json>    # Decrypt a nostr event
```

### Groups

```bash
ndr group create --name <name> --members <owner_pubkey_csv>
ndr group list
ndr group show <group_id>
ndr group update <group_id> [--name ...] [--description ...] [--picture ...]
ndr group add-member <group_id> <owner_pubkey>
ndr group remove-member <group_id> <owner_pubkey>
ndr group add-admin <group_id> <owner_pubkey>
ndr group remove-admin <group_id> <owner_pubkey>
ndr group accept <group_id>
ndr group send <group_id> <msg>
ndr group react <group_id> <msg_id> <emoji>
ndr group rotate-sender-key <group_id>
ndr group messages <group_id>
```

## Configuration

Default data directory: `~/.local/share/ndr/` (Linux) or platform equivalent.

Override with `--data-dir` flag or `NDR_DATA_DIR` environment variable.

Create `config.json` in data directory to configure relays:

```json
{
  "relays": ["wss://relay.example.com"]
}
```

## Examples

### Create an invite and wait for response

```bash
# Alice creates invite
ndr invite create
# Output: invite URL

# Alice listens for responses
ndr invite listen

# Bob joins (on his machine)
ndr chat join "https://..."

# Alice sees session created, can now send messages
ndr send <chat_id> "Hello Bob!"
```

### Send to npub using a public invite

```bash
# Bob publishes a public invite
ndr invite publish

# Alice can send directly to Bob's npub
ndr send npub1... "Hello Bob!"
```

### AI Agent Integration

```bash
# Agent receives message event from relay
event='{"kind":1060,"content":"...",...}'

# Decrypt and process
ndr --json receive "$event"
# Output: {"status":"ok","data":{"chat_id":"...","content":"Hello!"}}

# Reply
ndr --json send <chat_id> "I received your message"
```

## Security Notes

- Outer events are signature-verified; sender attribution is resolved from authenticated session context plus owner/device mapping.
- `ownerPubkey` claims are checked against AppKeys in multi-device flows.
- Inner rumor `pubkey` is not treated as trusted sender identity.
- Inner message payloads are unsigned and deniable; do not treat them as non-repudiable signatures.
