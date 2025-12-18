# Invite List Event - Design Specification

## Current Approach

Each device publishes its own replaceable event:
- **Kind:** 30078 (addressable/replaceable)
- **d-tag:** `double-ratchet/invites/{deviceId}`
- One event per device
- Device revocation = publish tombstone (same event without keys)

**Problems:**
- No atomicity across devices
- Each device independently manages its own invite
- No central authority controlling which devices exist
- Hard to implement "main device" that controls others

---

## Goals

### Primary Goals

1. **Atomicity** - All device invites in a single event, updated atomically
2. **Device revocation** - Any device with main nsec can revoke any device
3. **Discoverability** - Other users can fetch all your device invites in one query
4. **Recovery** - Backup main nsec = full recovery (no device-specific keys needed for list management)

### Secondary Goals

5. **Backwards compatibility** - Migration path from current per-device events
6. **Future-proof** - Architecture supports invite-only login apps (secondary devices that can chat but not modify list)

---

## Proposed Design: Invite List Event

### Event Structure

```json
{
  "kind": 10078,
  "pubkey": "<user's main pubkey>",
  "created_at": 1234567890,
  "tags": [
    ["d", "double-ratchet/invite-list"],
    ["device", "<ephemeralPubkey1>", "<sharedSecret1>", "<deviceId1>", "<deviceLabel1>"],
    ["device", "<ephemeralPubkey2>", "<sharedSecret2>", "<deviceId2>", "<deviceLabel2>"],
    ["version", "1"]
  ],
  "content": "",
  "sig": "<signature with main nsec>"
}
```

### Key Concepts

| Concept | Description |
|---------|-------------|
| **Main nsec** | User's primary Nostr private key, signs the invite list |
| **Invite key** | Ephemeral keypair for a specific device, used for handshakes. Can be rotated while keeping device ID and label the same. |
| **Shared secret** | Per-device secret for initial handshake encryption |
| **Device ID** | Unique identifier for the device |
| **Device label** | Human-readable name (e.g., "iPhone", "Laptop") |

---

## Key Hierarchy

```
Main nsec (full authority)
├── Signs invite list events
├── Can add/remove devices
└── Backup this for full recovery

Invite privkey (per device, stored locally)
├── Used for handshake responses
├── Never leaves the device
└── Losing device = lose this key (acceptable)
```

### Device Types

Currently, all iris-client devices have the main nsec (login requires it). All devices are equal and can modify the list.

| Type | Has Main nsec | Has Invite Key | Can Modify List |
|------|---------------|----------------|-----------------|
| Full device (current) | Yes | Yes | Yes |
| Invite-only device (future) | No | Yes (own only) | No |

**Future:** An invite-only login app could exist that only stores the device's invite privkey. It could chat but not modify the InviteList.

---

## Problem Scenarios

### 1. Data Conflicts (Race Conditions, Stale Cache, Relay Inconsistency)

**Scenario:** Multiple devices update the list simultaneously, or relays have different versions.

**Solution:** Union merge strategy (see Conflict Resolution Strategy section)
- Query multiple relays
- Publish to all known relays

### 2. Lost Device

**Scenario:** A device is lost/destroyed.

**If user has main nsec backed up:**
- Log into new device with nsec
- Revoke lost device from InviteList
- Full recovery ✓

**If user has NO backup of main nsec:**
- Cannot modify InviteList
- Existing sessions on other devices still work

**Mitigation:** Encourage nsec backup (standard Nostr recovery story)

### 3. Relay Unavailability

**Scenario:** Relays don't have or won't serve the invite list.

**Impact:** New contacts can't establish sessions (existing sessions unaffected).

**Mitigations:** Publish to multiple relays, direct invite sharing (URL/QR)

### 4. Event Size Limits

**Scenario:** User has many devices, event becomes too large.

**Mitigations:** Limit number of devices (e.g., max 10)

---

## Device Provisioning Flow

The secondary device generates its own keys; the main device only authorizes:

```
┌─────────────────────┐                    ┌─────────────────────┐
│   Secondary Device  │                    │    Main Device      │
└─────────────────────┘                    └─────────────────────┘
         │                                          │
         │ 1. Generate own invite:                  │
         │    - ephemeralKeypair                    │
         │    - sharedSecret                        │
         │    - deviceId                            │
         │                                          │
         │ 2. Display QR code containing:           │
         │    {ephemeralPubkey, sharedSecret,       │
         │     deviceId, deviceLabel}               │
         │                                          │
         │                    ◄──── 3. Scan QR ─────┤
         │                                          │
         │                         4. Fetch current │
         │                            invite list   │
         │                                          │
         │                         5. Add device    │
         │                            entry to list │
         │                                          │
         │                         6. Sign & publish│
         │                            updated list  │
         │                                          │
         ▼                                          ▼
   [Ready to accept                         [List updated]
    handshakes using
    own invite key]
```

**Benefits:**
- Secondary device's private key never leaves the device
- No sensitive data transmitted over network
- Works offline (QR is local transfer)

**What gets stored where:**

| Data | Own Device | Other Devices | Published List |
|------|------------|---------------|----------------|
| ephemeralPrivateKey | ✓ (local only) | ✗ | ✗ |
| ephemeralPublicKey | ✓ | ✓ | ✓ |
| sharedSecret | ✓ | ✓ | ✓ |
| deviceId | ✓ | ✓ | ✓ |
| deviceLabel | ✓ | ✓ | ✓ |
| main nsec | ✓ (iris-client) | ✓ | ✗ |

Note: In current iris-client, all devices have the main nsec. Future invite-only apps would not.

---

## Nostr Event Kind Reference

### Kind Number Conventions

| Range | Type | Behavior |
|-------|------|----------|
| 0-999 | Core | Protocol-defined |
| 1000-9999 | Regular | Stored, no replacement rules |
| 10000-19999 | Replaceable | One per pubkey, latest wins |
| 20000-29999 | Ephemeral | Not stored |
| 30000-39999 | Addressable | One per pubkey + d-tag combo |

### Double Ratchet Kinds

| Kind | Type | Why | NIP |
|------|------|-----|-----|
| **30078** | Addressable | Per-device invite (different d-tag per device) | NIP-78 (app data) |
| **10078** | Replaceable | Invite list (one per user) | NIP-78 (app data) |
| **1059** | Regular | Invite response (gift wrap) | NIP-59 |
| **1060** | Regular | Double ratchet encrypted messages | Custom |
| **14** | Regular | Chat message (wrapped in 1060) | NIP-17 |

### The "78" Convention

**NIP-78** defines kind 30078 for "arbitrary custom app data". We use the `78` suffix in both invite kinds:

- **30078** = addressable app data (one per d-tag) → per-device invites
- **10078** = replaceable app data (one per user) → invite list

---

## Conflict Resolution Strategy

**Core Rule: Union everything, then filter by removals.**

When there's inconsistency between local state and relay version:

1. **Union all device entries** from local + relay
2. **Union all removed entries** from local + relay
3. **Active devices = all devices − removed devices**

This means:
- No device is accidentally lost due to race conditions or stale data
- Removals are permanent and always respected (security-critical)
- A removed device cannot be re-added (would need a new deviceId)
