# Nostr Events Published by nostr-double-ratchet

## Event Kind Constants

```typescript
MESSAGE_EVENT_KIND = 1060
INVITE_EVENT_KIND = 30078
INVITE_RESPONSE_KIND = 1059
INVITE_LIST_EVENT_KIND = 10078
CHAT_MESSAGE_KIND = 14
```

---

## Session

### Kind 1060 - Encrypted Double-Ratchet Message

```json
{
  "kind": 1060,
  "pubkey": "<ourCurrentNostrKey.publicKey>",
  "content": "<nip44 encrypted rumor JSON>",
  "created_at": 1234567890,
  "tags": [
    ["header", "<nip44 encrypted header JSON>"]
  ]
}
```

**Header structure (encrypted in tag):**
```json
{
  "number": 0,
  "nextPublicKey": "<ourNextNostrKey.publicKey>",
  "previousChainLength": 0
}
```

**Inner event (rumor, encrypted in content):**
```json
{
  "id": "<event hash>",
  "pubkey": "0000000000000000000000000000000000000000000000000000000000000000",
  "content": "<message text>",
  "kind": 14,
  "created_at": 1234567890,
  "tags": [
    ["ms", "1234567890000"]
  ]
}
```

---

## Invite

### Kind 30078 - Device Invitation Event

```json
{
  "kind": 30078,
  "pubkey": "<inviter identity pubkey>",
  "content": "",
  "created_at": 1234567890,
  "tags": [
    ["ephemeralKey", "<inviterEphemeralPublicKey>"],
    ["sharedSecret", "<sharedSecret hex>"],
    ["d", "double-ratchet/invites/<deviceId>"],
    ["l", "double-ratchet/invites"]
  ]
}
```

### Kind 30078 - Device Revocation (Tombstone)

```json
{
  "kind": 30078,
  "pubkey": "<inviter identity pubkey>",
  "content": "",
  "created_at": 1234567890,
  "tags": [
    ["d", "double-ratchet/invites/<deviceId>"],
    ["l", "double-ratchet/invites"]
  ]
}
```

### Kind 1059 - Invite Response

```json
{
  "kind": 1059,
  "pubkey": "<random sender pubkey>",
  "content": "<nip44 encrypted inner event JSON>",
  "created_at": 1234567890,
  "tags": [
    ["p", "<inviterEphemeralPublicKey>"]
  ]
}
```

**Inner event (encrypted in content):**
```json
{
  "pubkey": "<invitee identity pubkey>",
  "content": "<nip44 encrypted with shared secret>",
  "created_at": 1234567890
}
```

**Payload (double-encrypted in inner content):**
```json
{
  "sessionKey": "<inviteeSessionPublicKey>",
  "deviceId": "<optional device id>"
}
```

---

## InviteList

### Kind 10078 - Consolidated Device Invite List

```json
{
  "kind": 10078,
  "pubkey": "<owner identity pubkey>",
  "content": "",
  "created_at": 1234567890,
  "tags": [
    ["d", "double-ratchet/invite-list"],
    ["version", "1"],
    ["device", "<ephemeralPublicKey>", "<sharedSecret>", "<deviceId>", "<createdAt>"],
    ["device", "<ephemeralPublicKey>", "<sharedSecret>", "<deviceId>", "<createdAt>", "<identityPubkey>"],
    ["removed", "<deviceId>"],
    ["removed", "<deviceId>"]
  ]
}
```

**Device tag format:**
```
["device", ephemeralPublicKey, sharedSecret, deviceId, createdAt, identityPubkey?]
```
- Index 0: `"device"`
- Index 1: Ephemeral public key (64 hex chars)
- Index 2: Shared secret (64 hex chars)
- Index 3: Device ID
- Index 4: Created at (unix timestamp string)
- Index 5: Identity pubkey (optional, for delegate devices)

**Removed tag format:**
```
["removed", deviceId]
```

### Kind 1059 - Invite Response (same as Invite)

```json
{
  "kind": 1059,
  "pubkey": "<random sender pubkey>",
  "content": "<nip44 encrypted inner event JSON>",
  "created_at": 1234567890,
  "tags": [
    ["p", "<device ephemeralPublicKey>"]
  ]
}
```

---

## Summary by Component

| Component | Kind | Event Type |
|-----------|------|------------|
| Session | 1060 | Encrypted double-ratchet message |
| Invite | 30078 | Device invitation |
| Invite | 30078 | Device revocation (tombstone) |
| Invite | 1059 | Invite acceptance response |
| InviteList | 10078 | Consolidated device invite list |
| InviteList | 1059 | Invite acceptance response |
