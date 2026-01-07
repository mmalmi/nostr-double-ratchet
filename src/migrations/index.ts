export type { Migration, MigrationContext } from "./runner"
export { runMigrations } from "./runner"
export { v0ToV1 } from "./v0ToV1"
export { v1ToV2 } from "./v1ToV2"

import type { Migration } from "./runner"
import { v0ToV1 } from "./v0ToV1"
import { v1ToV2 } from "./v1ToV2"

/**
 * All migrations in order. Add new migrations to the end of this array.
 */
export const migrations: Migration[] = [v0ToV1, v1ToV2]
