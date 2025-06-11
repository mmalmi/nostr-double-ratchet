/*
 * Simple async key	value storage interface plus an in	memory implementation.
 *
 * All methods are Promise	based to accommodate back	ends like
 * IndexedDB, SQLite, remote HTTP APIs, etc.  For environments where you only
 * need ephemeral data (tests, Node scripts) the InMemoryStorageAdapter can be
 * used directly.
 */

export interface StorageAdapter {
  /** Retrieve a value by key. */
  get<T = unknown>(key: string): Promise<T | undefined>
  /** Store a value by key. */
  put<T = unknown>(key: string, value: T): Promise<void>
  /** Delete a stored value by key. */
  del(key: string): Promise<void>
  /** List all keys that start with the given prefix. */
  list(prefix?: string): Promise<string[]>
}

export class InMemoryStorageAdapter implements StorageAdapter {
  private store = new Map<string, unknown>()

  async get<T = unknown>(key: string): Promise<T | undefined> {
    return this.store.get(key) as T | undefined
  }

  async put<T = unknown>(key: string, value: T): Promise<void> {
    this.store.set(key, value)
  }

  async del(key: string): Promise<void> {
    this.store.delete(key)
  }

  async list(prefix = ''): Promise<string[]> {
    const keys: string[] = []
    for (const k of this.store.keys()) {
      if (k.startsWith(prefix)) keys.push(k)
    }
    return keys
  }
} 