import type { Migration, MigrationContext } from "./runner"
import { Invite } from "../Invite"
import { InviteList } from "../InviteList"
import { INVITE_LIST_EVENT_KIND } from "../types"

/**
 * Migration v1 → v2
 *
 * Changes:
 * - Consolidate per-device invites (kind 30078) into a single InviteList (kind 10078)
 * - Move storage keys from v1/* to v2/* for compatibility with v2 SessionManager
 * - Tombstone our old per-device invite for backwards-compat signaling
 */
export const v1ToV2: Migration = {
  name: "v1ToV2",
  fromVersion: "1",
  toVersion: "2",

  async migrate(ctx: MigrationContext): Promise<void> {
    const { storage, deviceId, ourPublicKey, nostrSubscribe, nostrPublish } = ctx

    // 1) Move user records and per-user invites forward from v1 -> v2
    await movePrefix(storage, "v1/user/", "v2/user/")
    await movePrefix(storage, "v1/invite/", "v2/invite/")
    await movePrefix(storage, "v1/invite-accept/", "v2/invite-accept/")

    // 2) Build local InviteList from our v1 device invite, if present
    const v1DeviceInviteKey = `v1/device-invite/${deviceId}`
    const serializedInvite = await storage.get<string>(v1DeviceInviteKey)

    let localList: InviteList | null = null
    let legacyInvite: Invite | null = null
    if (serializedInvite) {
      try {
        legacyInvite = Invite.deserialize(serializedInvite)
        // Convert Invite -> DeviceEntry
        const device = {
          ephemeralPublicKey: legacyInvite.inviterEphemeralPublicKey,
          ephemeralPrivateKey: legacyInvite.inviterEphemeralPrivateKey,
          sharedSecret: legacyInvite.sharedSecret,
          deviceId: legacyInvite.deviceId || deviceId,
          deviceLabel: legacyInvite.deviceId || deviceId,
          createdAt: legacyInvite.createdAt,
        }
        localList = new InviteList(ourPublicKey, [device])
      } catch (e) {
        // Ignore malformed legacy invite
        console.warn("v1→v2: failed to deserialize device invite:", e)
      }
    }

    // 3) Fetch existing InviteList from relay (another device may have already migrated)
    const remoteList = await fetchInviteList(ourPublicKey, nostrSubscribe)

    // 4) Merge (union) local + remote
    const merged = mergeLists(localList, remoteList, ourPublicKey)

    // 5) Save & publish InviteList under v2
    await storage.put("v2/invite-list", merged.serialize())
    try {
      await nostrPublish(merged.getEvent())
    } catch (e) {
      // Non-fatal: storage still contains latest list
      console.warn("v1→v2: failed to publish InviteList:", e)
    }

    // 6) Tombstone our legacy per-device invite to signal revocation to older clients
    if (legacyInvite) {
      try {
        await nostrPublish(legacyInvite.getDeletionEvent())
      } catch (e) {
        console.warn("v1→v2: failed to publish legacy invite tombstone:", e)
      }
    }

    // 7) Cleanup legacy device invite
    await storage.del(v1DeviceInviteKey)
  },
}

async function movePrefix(
  storage: MigrationContext["storage"],
  fromPrefix: string,
  toPrefix: string
) {
  const keys = await storage.list(fromPrefix)
  if (keys.length === 0) return

  await Promise.all(
    keys.map(async (key) => {
      try {
        const suffix = key.slice(fromPrefix.length)
        const value = await storage.get(key)
        if (typeof value !== "undefined") {
          await storage.put(toPrefix + suffix, value)
        }
        await storage.del(key)
      } catch (e) {
        console.warn(`v1→v2: failed moving ${key} to ${toPrefix}:`, e)
      }
    })
  )
}

function mergeLists(
  localList: InviteList | null,
  remoteList: InviteList | null,
  ownerPublicKey: string
): InviteList {
  if (localList && remoteList) return localList.merge(remoteList)
  if (localList) return localList
  if (remoteList) return remoteList
  return new InviteList(ownerPublicKey)
}

async function fetchInviteList(
  ownerPublicKey: string,
  subscribe: MigrationContext["nostrSubscribe"],
  timeoutMs = 500
): Promise<InviteList | null> {
  return new Promise((resolve) => {
    let resolved = false
    let found: InviteList | null = null

    const timeout = setTimeout(() => {
      if (resolved) return
      resolved = true
      unsubscribe()
      resolve(found)
    }, timeoutMs)

    let unsubscribe: () => void = () => {}
    unsubscribe = subscribe(
      {
        kinds: [INVITE_LIST_EVENT_KIND],
        authors: [ownerPublicKey],
        "#d": ["double-ratchet/invite-list"],
        limit: 1,
      },
      (event) => {
        if (resolved) return
        try {
          found = InviteList.fromEvent(event)
          resolved = true
          clearTimeout(timeout)
          unsubscribe()
          resolve(found)
        } catch {
          // ignore invalid events
        }
      }
    )
  })
}
