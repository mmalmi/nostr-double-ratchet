import { StorageAdapter } from "./StorageAdapter"
import { Rumor } from "./types"

export interface QueueEntry {
  id: string
  targetKey: string
  event: Rumor
  createdAt: number
}

export class MessageQueue {
  private storage: StorageAdapter
  private prefix: string

  constructor(storage: StorageAdapter, prefix: string) {
    this.storage = storage
    this.prefix = prefix
  }

  async add(targetKey: string, event: Rumor): Promise<string> {
    const id = crypto.randomUUID()
    const entry: QueueEntry = { id, targetKey, event, createdAt: Date.now() }
    await this.storage.put(`${this.prefix}${id}`, entry)
    return id
  }

  async getForTarget(targetKey: string): Promise<QueueEntry[]> {
    const keys = await this.storage.list(this.prefix)
    const entries: QueueEntry[] = []
    const seenEventIds = new Set<string>()
    for (const key of keys) {
      const entry = await this.storage.get<QueueEntry>(key)
      if (entry && entry.targetKey === targetKey && !seenEventIds.has(entry.event.id)) {
        seenEventIds.add(entry.event.id)
        entries.push(entry)
      }
    }
    return entries.sort((a, b) => a.createdAt - b.createdAt)
  }

  async removeForTarget(targetKey: string): Promise<void> {
    const keys = await this.storage.list(this.prefix)
    for (const key of keys) {
      const entry = await this.storage.get<QueueEntry>(key)
      if (entry && entry.targetKey === targetKey) {
        await this.storage.del(key)
      }
    }
  }

  async remove(id: string): Promise<void> {
    await this.storage.del(`${this.prefix}${id}`)
  }
}
