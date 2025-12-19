# Invite List - Implementation Architecture

## Layers

### Layer 1: inviteUtils.ts (NEW)

Extract from Invite.ts - pure functions, no I/O:
- `generateEphemeralKeypair()`, `generateSharedSecret()`, `generateDeviceId()`
- `encryptInviteResponse()`, `decryptInviteResponse()`
- `createSessionFromAccept()`

### Layer 2a: Invite.ts (REFACTOR)

Per-device invite (kind 30078) - extract shared logic to inviteUtils.ts

### Layer 2b: InviteList.ts (NEW)

Consolidated invite list (kind 10078):
- `fromEvent()`, `getEvent()`, `fetch()`, `subscribe()`
- `addDevice()`, `removeDevice()`, `getDevice()`, `getAllDevices()`
- `merge()` - conflict resolution
- `accept()`, `listen()` - handshake using shared utils

### Layer 3: SessionManager.ts (MODIFY)

- Use InviteList instead of per-device Invite
- Backwards compat: read InviteList first, fall back to per-device
- Write InviteList only

### Layer 4: iris-client (MINIMAL)

- Device listing page
- Device management UI (add/remove/edit label)

---

## Phases

1. **Extract Shared Utils** - inviteUtils.ts + tests
2. **InviteList Core** - InviteList.ts + tests
3. **SessionManager Integration** - backwards compatible
4. **iris-client UI** - device management pages
5. **Cleanup** - remove per-device support
6. **Invite-Only App** (future) - login with only invite privkey
