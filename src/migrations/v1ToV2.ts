import type { Migration, MigrationContext } from "./runner"
import { Invite } from "../Invite"
import { InviteList, DeviceEntry } from "../InviteList"
import { INVITE_LIST_EVENT_KIND } from "../types"

// Storage keys
const V1_DEVICE_INVITE_PREFIX = "v1/device-invite/"
const V2_INVITE_LIST_KEY = "v2/invite-list"

/**
 * Migration v1 → v2: Per-device invites to consolidated InviteList
 *
 * - Converts per-device invite (kind 30078) to InviteList (kind 10078)
 * - Each device only migrates itself
 * - Publishes tombstone for old per-device invite
 * - Old devices continue working until they upgrade
 */
export const v1ToV2: Migration = {
  name: "v1ToV2",
  fromVersion: "1",
  toVersion: "2",

  async migrate(ctx: MigrationContext): Promise<void> {
    const { storage, deviceId, ourPublicKey, nostrSubscribe, nostrPublish } = ctx

    // 1. Load our device invite from v1 storage
    const v1DeviceInviteKey = V1_DEVICE_INVITE_PREFIX + deviceId
    const inviteData = await storage.get<string>(v1DeviceInviteKey)
    if (!inviteData) {
      // Nothing to migrate - this device never had a v1 invite
      return
    }

    let ourInvite: Invite
    try {
      ourInvite = Invite.deserialize(inviteData)
    } catch {
      console.error("Failed to deserialize v1 invite during migration")
      return
    }

    // Convert invite to device entry
    const deviceEntry: DeviceEntry = {
      ephemeralPublicKey: ourInvite.inviterEphemeralPublicKey,
      ephemeralPrivateKey: ourInvite.inviterEphemeralPrivateKey,
      sharedSecret: ourInvite.sharedSecret,
      deviceId: ourInvite.deviceId || deviceId,
      deviceLabel: ourInvite.deviceId || deviceId,
      createdAt: ourInvite.createdAt,
    }

    // 2. Fetch existing InviteList from relay (another device may have already migrated)
    const remoteList = await fetchUserInviteList(ourPublicKey, nostrSubscribe)

    // 3. Build InviteList
    let inviteList: InviteList
    if (remoteList) {
      // Merge our device into existing list
      const localList = new InviteList(ourPublicKey, [deviceEntry])
      inviteList = remoteList.merge(localList)
    } else {
      // Create new list with just our device
      inviteList = new InviteList(ourPublicKey, [deviceEntry])
    }

    // 4. Publish InviteList (kind 10078)
    const inviteListEvent = inviteList.getEvent()
    await nostrPublish(inviteListEvent).catch((error) => {
      console.error("Failed to publish InviteList during migration:", error)
    })

    // 5. Publish tombstone for our old per-device invite (kind 30078)
    if (ourInvite.deviceId) {
      const tombstone = ourInvite.getDeletionEvent()
      await nostrPublish(tombstone).catch((error) => {
        console.error("Failed to publish tombstone during migration:", error)
      })
    }

    // 6. Save InviteList to local storage (with v2 prefix)
    await storage.put(V2_INVITE_LIST_KEY, inviteList.serialize())

    // 7. Delete old v1 storage key
    await storage.del(v1DeviceInviteKey)
  },
}

/**
 * Fetches a user's InviteList from the relay with timeout.
 */
function fetchUserInviteList(
  pubkey: string,
  nostrSubscribe: MigrationContext["nostrSubscribe"],
  timeoutMs: number = 500
): Promise<InviteList | null> {
  return new Promise((resolve) => {
    let found: InviteList | null = null
    let resolved = false

    const timeout = setTimeout(() => {
      if (resolved) return
      resolved = true
      unsubscribe()
      resolve(found)
    }, timeoutMs)

    let unsubscribe: () => void = () => {}
    unsubscribe = nostrSubscribe(
      {
        kinds: [INVITE_LIST_EVENT_KIND],
        authors: [pubkey],
        "#d": ["double-ratchet/invite-list"],
        limit: 1,
      },
      (event) => {
        if (resolved) return
        try {
          found = InviteList.fromEvent(event)
          resolved = true
          clearTimeout(timeout)
          resolve(found)
        } catch {
          // Invalid event, ignore
        }
      }
    )

    // If we found the event synchronously, unsubscribe now
    if (resolved) {
      unsubscribe()
    }
  })
}
