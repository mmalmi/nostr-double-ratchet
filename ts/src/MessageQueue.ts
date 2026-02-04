import { StorageAdapter } from "./StorageAdapter"
import { Rumor } from "./types"

/**
 * Persistent queue item for messages waiting to be sent.
 * Stored immediately when sendEvent is called, ensuring no data loss on crash.
 */
export interface StoredQueueItem {
  id: string // rumor.id
  rumor: Rumor // The complete event to send
  recipientOwnerPubkey: string // Recipient's owner pubkey (not device key)
  queuedAt: number // When message was queued
  deviceStatus: Record<string, { sent: boolean; sentAt?: number }> // Per-device status
  targetDevices: string[] // All devices that need to receive this message
}

export interface MessageQueueOptions {
  storage: StorageAdapter
  versionPrefix: string
}

/**
 * Message queue for managing outgoing messages with per-device delivery tracking.
 * Handles persistence of queue items to storage, allowing recovery after crashes.
 */
export class MessageQueue {
  /** Minimum time (ms) to keep a queue item before dequeueing */
  static readonly HOLD_TIME_MS = 500

  private storage: StorageAdapter
  private versionPrefix: string

  constructor(options: MessageQueueOptions) {
    this.storage = options.storage
    this.versionPrefix = options.versionPrefix
  }

  private queueKeyPrefix(): string {
    return `${this.versionPrefix}/queue/`
  }

  private queueItemKey(messageId: string): string {
    return `${this.queueKeyPrefix()}${messageId}`
  }

  /**
   * Add a message to the queue.
   * Creates device status entries for all target devices.
   */
  async enqueue(
    rumor: Rumor,
    recipientOwnerPubkey: string,
    targetDevices: string[]
  ): Promise<StoredQueueItem> {
    const deviceStatus: Record<string, { sent: boolean; sentAt?: number }> = {}
    for (const deviceId of targetDevices) {
      deviceStatus[deviceId] = { sent: false }
    }

    const queueItem: StoredQueueItem = {
      id: rumor.id,
      rumor,
      recipientOwnerPubkey,
      queuedAt: Date.now(),
      deviceStatus,
      targetDevices,
    }

    await this.storage.put(this.queueItemKey(rumor.id), queueItem)
    return queueItem
  }

  /**
   * Remove a message from the queue.
   */
  async dequeue(messageId: string): Promise<void> {
    await this.storage.del(this.queueItemKey(messageId))
  }

  /**
   * Get a specific queue item by message ID.
   */
  async getItem(messageId: string): Promise<StoredQueueItem | undefined> {
    return this.storage.get<StoredQueueItem>(this.queueItemKey(messageId))
  }

  /**
   * Load all queued messages from storage, sorted by queuedAt (oldest first).
   */
  async loadAll(): Promise<StoredQueueItem[]> {
    const prefix = this.queueKeyPrefix()
    const keys = await this.storage.list(prefix)
    const items: StoredQueueItem[] = []

    for (const key of keys) {
      const item = await this.storage.get<StoredQueueItem>(key)
      if (item) {
        items.push(item)
      }
    }

    // Sort by queuedAt to process oldest first
    items.sort((a, b) => a.queuedAt - b.queuedAt)
    return items
  }

  /**
   * Update the delivery status for a specific device in a queue item.
   */
  async updateDeviceStatus(
    messageId: string,
    deviceId: string,
    sent: boolean
  ): Promise<void> {
    const queueItem = await this.storage.get<StoredQueueItem>(
      this.queueItemKey(messageId)
    )
    if (!queueItem) return

    queueItem.deviceStatus[deviceId] = {
      sent,
      sentAt: sent ? Date.now() : undefined,
    }

    await this.storage.put(this.queueItemKey(messageId), queueItem)
  }

  /**
   * Add a newly discovered device to an existing queue item.
   * No-op if device is already in target devices.
   */
  async addDeviceToItem(messageId: string, deviceId: string): Promise<void> {
    const queueItem = await this.storage.get<StoredQueueItem>(
      this.queueItemKey(messageId)
    )
    if (!queueItem) return

    // Only add if not already in target devices
    if (!queueItem.targetDevices.includes(deviceId)) {
      queueItem.targetDevices.push(deviceId)
      queueItem.deviceStatus[deviceId] = { sent: false }
      await this.storage.put(this.queueItemKey(messageId), queueItem)
    }
  }

  /**
   * Check if all target devices have received the message.
   */
  isComplete(item: StoredQueueItem): boolean {
    return item.targetDevices.every(
      (deviceId) => item.deviceStatus[deviceId]?.sent === true
    )
  }

  /**
   * Check if a queue item is ready for dequeue (complete and past hold time).
   */
  isReadyForDequeue(item: StoredQueueItem): boolean {
    if (!this.isComplete(item)) {
      return false
    }
    const age = Date.now() - item.queuedAt
    return age >= MessageQueue.HOLD_TIME_MS
  }

  /**
   * Get all queue items for a specific recipient.
   */
  async getItemsForRecipient(ownerPubkey: string): Promise<StoredQueueItem[]> {
    const allItems = await this.loadAll()
    return allItems.filter((item) => item.recipientOwnerPubkey === ownerPubkey)
  }

  /**
   * Delete all queue items for a specific recipient.
   */
  async deleteItemsForRecipient(ownerPubkey: string): Promise<void> {
    const items = await this.getItemsForRecipient(ownerPubkey)
    for (const item of items) {
      await this.dequeue(item.id)
    }
  }
}
