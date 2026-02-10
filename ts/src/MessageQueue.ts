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
    const id = event.id + "/" + targetKey
    const entry: QueueEntry = { id, targetKey, event, createdAt: Date.now() }
    await this.storage.put(`${this.prefix}${id}`, entry)
    console.log(`[MQ:${this.prefix}] add target=${targetKey.slice(0,8)} event=${event.id?.slice(0,8)} content="${event.content?.slice(0,30)}"`)
    return id
  }

  async getForTarget(targetKey: string): Promise<QueueEntry[]> {
    const keys = await this.storage.list(this.prefix)
    const entries: QueueEntry[] = []
    for (const key of keys) {
      const entry = await this.storage.get<QueueEntry>(key)
      if (entry && entry.targetKey === targetKey) {
        entries.push(entry)
      }
    }
    const sorted = entries.sort((a, b) => a.createdAt - b.createdAt)
    console.log(`[MQ:${this.prefix}] getForTarget target=${targetKey.slice(0,8)} found=${sorted.length} totalKeys=${keys.length}`)
    return sorted
  }

  async removeForTarget(targetKey: string): Promise<void> {
    const keys = await this.storage.list(this.prefix)
    let removed = 0
    for (const key of keys) {
      const entry = await this.storage.get<QueueEntry>(key)
      if (entry && entry.targetKey === targetKey) {
        await this.storage.del(key)
        removed++
      }
    }
    console.log(`[MQ:${this.prefix}] removeForTarget target=${targetKey.slice(0,8)} removed=${removed}`)
  }

  async removeByTargetAndEventId(targetKey: string, eventId: string): Promise<void> {
    await this.remove(eventId + "/" + targetKey)
  }

  async remove(id: string): Promise<void> {
    console.log(`[MQ:${this.prefix}] remove id=${id.slice(0,12)}`)
    await this.storage.del(`${this.prefix}${id}`)
  }
}
