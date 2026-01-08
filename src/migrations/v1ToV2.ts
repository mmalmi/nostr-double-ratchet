import type { Migration, MigrationContext } from "./runner"

/**
 * Migration v1 → v2: Per-device invites to consolidated InviteList
 *
 * TODO: Implement migration that preserves existing chats
 */
export const v1ToV2: Migration = {
  name: "v1ToV2",
  fromVersion: "1",
  toVersion: "2",

  async migrate(_ctx: MigrationContext): Promise<void> {
    // No-op for now - migration logic to be implemented
  },
}
