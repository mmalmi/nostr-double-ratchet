import type { Migration, MigrationContext } from "./runner"
import { Invite } from "../Invite"

// Minimal type for reading old user records
interface LegacyUserRecord {
  publicKey: string
  devices: Array<{
    deviceId: string
    createdAt: number
  }>
}

// Storage keys for v0 (no prefix)
const V0_INVITE_PREFIX = "invite/"
const V0_USER_RECORD_PREFIX = "user/"

// Storage keys for v1
const V1_INVITE_PREFIX = "v1/invite/"
const V1_USER_RECORD_PREFIX = "v1/user/"

/**
 * Migration v0 → v1: Storage key restructuring
 *
 * - Moves invites from `invite/{pubkey}` to `v1/invite/{pubkey}`
 * - Moves user records from `user/{pubkey}` to `v1/user/{pubkey}`
 * - Clears old sessions (they had key issues in v0)
 */
export const v0ToV1: Migration = {
  name: "v0ToV1",
  fromVersion: null,
  toVersion: "1",

  async migrate(ctx: MigrationContext): Promise<void> {
    const { storage } = ctx

    // Migrate invites: re-serialize to get persistent createdAt
    const inviteKeys = await storage.list(V0_INVITE_PREFIX)
    await Promise.all(
      inviteKeys.map(async (key) => {
        try {
          const publicKey = key.slice(V0_INVITE_PREFIX.length)
          const inviteData = await storage.get<string>(key)
          if (inviteData) {
            const invite = Invite.deserialize(inviteData)
            const newKey = V1_INVITE_PREFIX + publicKey
            await storage.put(newKey, invite.serialize())
            await storage.del(key)
          }
        } catch (e) {
          console.error("Migration v0→v1 error for invite:", e)
        }
      })
    )

    // Migrate user records: clear sessions (had key issues)
    const userRecordKeys = await storage.list(V0_USER_RECORD_PREFIX)
    await Promise.all(
      userRecordKeys.map(async (key) => {
        try {
          const publicKey = key.slice(V0_USER_RECORD_PREFIX.length)
          const userRecordData = await storage.get<LegacyUserRecord>(key)
          if (userRecordData) {
            const newKey = V1_USER_RECORD_PREFIX + publicKey
            // Clear sessions but preserve device metadata
            const newUserRecordData = {
              publicKey: userRecordData.publicKey,
              devices: userRecordData.devices.map((device) => ({
                deviceId: device.deviceId,
                activeSession: null,
                createdAt: device.createdAt,
                inactiveSessions: [],
              })),
            }
            await storage.put(newKey, newUserRecordData)
            await storage.del(key)
          }
        } catch (e) {
          console.error("Migration v0→v1 error for user record:", e)
        }
      })
    )
  },
}
