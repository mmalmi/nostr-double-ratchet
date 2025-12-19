# InviteList Migration Strategy

## Overview

Migration from per-device invites (kind 30078) to consolidated InviteList (kind 10078).

**Key principle:** Each device only migrates itself. Old devices continue working until they upgrade.

---

## Migration v1 → v2

Triggered once per device when SessionManager initializes with storage version "1".

### Steps

1. **Load own device invite** from storage (has ephemeralPrivateKey)
2. **Fetch existing InviteList** from relay (kind 10078)
   - Another device may have already migrated
3. **Build InviteList:**
   - If remote InviteList exists: merge our device into it
   - If not: create new InviteList with just our device
4. **Publish InviteList** (kind 10078)
5. **Publish tombstone** for only our per-device invite (kind 30078)
6. **Save InviteList** to local storage
7. **Delete old storage key** (`v1/device-invite/{deviceId}`)
8. **Set storage version** to "2"

### Pseudocode

```typescript
async function migrateV1ToV2() {
  // 1. Load our invite
  const ourInvite = await storage.get(deviceInviteKey(this.deviceId))
  if (!ourInvite) return // Nothing to migrate

  const device: DeviceEntry = {
    ephemeralPublicKey: ourInvite.inviterEphemeralPublicKey,
    ephemeralPrivateKey: ourInvite.inviterEphemeralPrivateKey,
    sharedSecret: ourInvite.sharedSecret,
    deviceId: ourInvite.deviceId,
    deviceLabel: ourInvite.deviceId, // Default label
    createdAt: ourInvite.createdAt,
  }

  // 2. Fetch existing InviteList from relay
  const remoteList = await fetchInviteList(this.ourPublicKey)

  // 3. Build merged list
  let inviteList: InviteList
  if (remoteList) {
    inviteList = remoteList
    inviteList.addDevice(device)
  } else {
    inviteList = new InviteList(this.ourPublicKey, [device])
  }

  // 4. Publish InviteList
  await this.nostrPublish(signEvent(inviteList.getEvent()))

  // 5. Publish tombstone for our old invite
  await this.nostrPublish(signEvent(ourInvite.getDeletionEvent()))

  // 6. Save locally
  await storage.put(inviteListKey(), inviteList.serialize())

  // 7. Delete old key
  await storage.del(deviceInviteKey(this.deviceId))

  // 8. Update version
  await storage.put(versionKey(), "2")
}
```

---

## Post-Migration Behavior

### Reading Other Users' Invites

```
1. Try to fetch InviteList (kind 10078)
2. If found: use devices from InviteList
3. If not found: fall back to per-device invites (kind 30078)
4. Subscribe to both for updates
```

### Modifying Our InviteList

**Critical invariant:** Always fetch-merge-publish to avoid dropping devices.

```typescript
async function modifyInviteList(change: (list: InviteList) => void) {
  // Always fetch latest from relay first
  const remote = await fetchInviteList(this.ourPublicKey)

  // Merge with local (preserves private keys)
  const merged = this.inviteList.merge(remote)

  // Apply the change
  change(merged)

  // Publish and save
  await this.nostrPublish(signEvent(merged.getEvent()))
  await storage.put(inviteListKey(), merged.serialize())

  this.inviteList = merged
}

// Usage
await modifyInviteList(list => list.removeDevice(deviceId))
await modifyInviteList(list => list.addDevice(newDevice))
```

---

## Multi-Device Scenarios

### Scenario 1: First Device Migrates

```
Device A (v1 → v2):
1. No InviteList on relay
2. Creates InviteList with device A only
3. Publishes InviteList
4. Tombstones its per-device invite

Device B (still v1):
- Continues using per-device invite
- Unaffected by A's migration
```

### Scenario 2: Second Device Migrates

```
Device B (v1 → v2):
1. Fetches InviteList (contains device A)
2. Adds device B to list
3. Publishes updated InviteList (A + B)
4. Tombstones its per-device invite

Result: InviteList now has both devices
```

### Scenario 3: Race Condition

```
Device A and B migrate simultaneously:

Device A:
1. Fetches InviteList (empty)
2. Creates list with A
3. Publishes

Device B:
1. Fetches InviteList (empty - before A published)
2. Creates list with B
3. Publishes (overwrites A's)

Problem: A is lost!
```

**Solution:** On next modification, always fetch and merge:
- Device A modifies list → fetches (has B) → merges (A + B) → publishes
- Union merge strategy ensures no device is permanently lost

### Scenario 4: Offline Device

```
Device A: migrates, creates InviteList
Device B: offline for weeks

When B comes online:
1. B's per-device invite still works (not tombstoned)
2. B migrates, fetches InviteList, merges itself in
3. Both devices now in InviteList
```

---

## Storage Keys

| Version | Key Pattern | Content |
|---------|-------------|---------|
| v1 | `v1/device-invite/{deviceId}` | Serialized Invite |
| v2 | `v2/invite-list` | Serialized InviteList |

---

## Relay Interaction

### Fetching InviteList

```typescript
async function fetchInviteList(pubkey: string): Promise<InviteList | null> {
  return new Promise((resolve) => {
    let found: InviteList | null = null
    const timeout = setTimeout(() => {
      unsub()
      resolve(found)
    }, 3000)

    const unsub = nostrSubscribe(
      {
        kinds: [INVITE_LIST_EVENT_KIND],
        authors: [pubkey],
        "#d": ["double-ratchet/invite-list"],
        limit: 1,
      },
      (event) => {
        found = InviteList.fromEvent(event)
        clearTimeout(timeout)
        unsub()
        resolve(found)
      }
    )
  })
}
```

### Subscribing to Updates

Subscribe to both kinds during transition:

```typescript
function subscribeToUserInvites(pubkey: string, onDevice: (device) => void) {
  // New: InviteList
  const unsub1 = nostrSubscribe(
    { kinds: [INVITE_LIST_EVENT_KIND], authors: [pubkey] },
    (event) => {
      const list = InviteList.fromEvent(event)
      for (const device of list.getAllDevices()) {
        onDevice(device)
      }
    }
  )

  // Legacy: per-device invites
  const unsub2 = Invite.fromUser(pubkey, nostrSubscribe, (invite) => {
    onDevice({
      ephemeralPublicKey: invite.inviterEphemeralPublicKey,
      sharedSecret: invite.sharedSecret,
      deviceId: invite.deviceId,
      // ...
    })
  })

  return () => { unsub1(); unsub2() }
}
```

---

## Cleanup (Phase 5)

After all clients have migrated (determined by adoption metrics):

1. Remove `Invite.fromUser()` fallback
2. Remove per-device invite subscription
3. Remove tombstone handling for kind 30078
4. Simplify to InviteList-only code path
