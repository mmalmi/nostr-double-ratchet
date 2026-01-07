import type { StorageAdapter } from "../StorageAdapter"
import type { NostrSubscribe, NostrPublish } from "../types"

export interface MigrationContext {
  storage: StorageAdapter
  deviceId: string
  ourPublicKey: string
  nostrSubscribe: NostrSubscribe
  nostrPublish: NostrPublish
}

export interface Migration {
  name: string
  fromVersion: string | null // null means "no version set"
  toVersion: string
  migrate: (ctx: MigrationContext) => Promise<void>
}

const VERSION_KEY = "storage-version"

/**
 * Runs migrations sequentially based on the current storage version.
 *
 * Each migration specifies its fromVersion and toVersion. The runner
 * executes migrations in order, updating the version after each one.
 */
export async function runMigrations(
  ctx: MigrationContext,
  migrations: Migration[]
): Promise<void> {
  let currentVersion = await ctx.storage.get<string>(VERSION_KEY)

  for (const migration of migrations) {
    const shouldRun =
      migration.fromVersion === null
        ? !currentVersion
        : currentVersion === migration.fromVersion

    if (shouldRun) {
      await migration.migrate(ctx)
      currentVersion = migration.toVersion
      await ctx.storage.put(VERSION_KEY, currentVersion)
    }
  }
}
