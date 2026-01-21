/*
 * Simple async key-value storage interface plus implementations.
 *
 * All methods are Promise-based to accommodate back-ends like
 * IndexedDB, SQLite, remote HTTP APIs, etc. For environments where you only
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

  async list(prefix = ""): Promise<string[]> {
    const keys: string[] = []
    const storeKeys = Array.from(this.store.keys())
    for (const k of storeKeys) {
      if (k.startsWith(prefix)) keys.push(k)
    }
    return keys
  }
}

export class LocalStorageAdapter implements StorageAdapter {
  private keyPrefix: string

  constructor(keyPrefix = "session_") {
    this.keyPrefix = keyPrefix
  }

  private getFullKey(key: string): string {
    return `${this.keyPrefix}${key}`
  }

  async get<T = unknown>(key: string): Promise<T | undefined> {
    try {
      const item = localStorage.getItem(this.getFullKey(key))
      return item ? JSON.parse(item) : undefined
    } catch {
      return undefined
    }
  }

  async put<T = unknown>(key: string, value: T): Promise<void> {
    try {
      localStorage.setItem(this.getFullKey(key), JSON.stringify(value))
    } catch (e) {
      console.error(`Failed to put key ${key} to localStorage:`, e)
      throw e
    }
  }

  async del(key: string): Promise<void> {
    try {
      localStorage.removeItem(this.getFullKey(key))
    } catch {
      // Ignore deletion failures
    }
  }

  async list(prefix = ""): Promise<string[]> {
    const keys: string[] = []
    const searchPrefix = this.getFullKey(prefix)

    try {
      for (let i = 0; i < localStorage.length; i++) {
        const key = localStorage.key(i)
        if (key && key.startsWith(searchPrefix)) {
          // Remove our prefix to return the original key
          keys.push(key.substring(this.keyPrefix.length))
        }
      }
    } catch {
      // Ignore list failures
    }

    return keys
  }
}
