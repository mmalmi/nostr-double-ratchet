import { matchFilter, VerifiedEvent, UnsignedEvent, Filter } from "nostr-tools"
import { NDKEvent, NDKPrivateKeySigner } from "@nostr-dev-kit/ndk"

// ============================================================================
// Types
// ============================================================================

export interface ControlledMockRelayOptions {
  /** Enable debug logging */
  debug?: boolean
  /** Auto-deliver events after this delay (ms). Default: manual only (undefined) */
  autoDeliveryDelay?: number
  /** Enable cascading replay - replay to new subscriptions created during event processing (default: true) */
  cascadeReplay?: boolean
}

export interface PendingEvent {
  event: VerifiedEvent
  receivedAt: number
  /** If set, only deliver to these subscriber IDs */
  targetSubscribers?: string[]
}

export interface DeliveryRecord {
  eventId: string
  subscriberId: string
  timestamp: number
  filters: Filter[]
}

export interface SubscriptionInfo {
  id: string
  filters: Filter[]
  createdAt: number
  eoseSent: boolean
}

export interface Subscription {
  id: string
  filters: Filter[]
  onEvent: (event: VerifiedEvent) => void
  onEose?: () => void
  createdAt: number
  delivered: Set<string>
  eoseSent: boolean
  closed: boolean
}

export interface SubscriptionHandle {
  id: string
  close: () => void
}

// ============================================================================
// ControlledMockRelay
// ============================================================================

export class ControlledMockRelay {
  // Storage
  private pendingEvents: PendingEvent[] = []
  private deliveredEvents: VerifiedEvent[] = []
  private subscriptions: Map<string, Subscription> = new Map()
  private deliveryHistory: DeliveryRecord[] = []

  // Counters
  private subscriptionCounter = 0
  private eventCounter = 0

  // Config
  private debug: boolean
  private autoDeliveryDelay?: number
  private autoEose: boolean = false
  private cascadeReplay: boolean

  constructor(options: ControlledMockRelayOptions = {}) {
    this.debug = options.debug ?? false
    this.autoDeliveryDelay = options.autoDeliveryDelay
    this.cascadeReplay = options.cascadeReplay ?? true
  }

  // ============================================================================
  // Publishing
  // ============================================================================

  /**
   * Publish an event to the relay. The event is added to the pending queue
   * and will only be delivered when explicitly triggered.
   * @returns The event ID
   */
  async publish(
    event: UnsignedEvent,
    signerSecretKey?: Uint8Array
  ): Promise<string> {
    const verifiedEvent = await this.signEvent(event, signerSecretKey)

    this.pendingEvents.push({
      event: verifiedEvent,
      receivedAt: Date.now(),
    })

    this.log(`Event ${verifiedEvent.id} added to pending queue (${this.pendingEvents.length} pending)`)

    if (this.autoDeliveryDelay !== undefined) {
      setTimeout(() => {
        this.deliverEvent(verifiedEvent.id)
      }, this.autoDeliveryDelay)
    }

    return verifiedEvent.id
  }

  /**
   * Publish an event and immediately deliver it to all matching subscribers.
   * @returns The event ID
   */
  async publishAndDeliver(
    event: UnsignedEvent,
    signerSecretKey?: Uint8Array
  ): Promise<string> {
    const verifiedEvent = await this.signEvent(event, signerSecretKey)

    this.deliveredEvents.push(verifiedEvent)
    const activeSubs = Array.from(this.subscriptions.values()).filter(s => !s.closed)
    const subFilters = activeSubs.map(s => s.filters.map(f => f.authors?.map(a => a.slice(0, 8)).join(',') || 'none').join(';')).join(' | ')
    console.warn(`[ControlledMockRelay] Event (pubkey=${verifiedEvent.pubkey?.slice(0, 8)}) published, ${activeSubs.length} subs: [${subFilters}]`)

    for (const sub of this.subscriptions.values()) {
      if (!sub.closed) {
        this.deliverEventToSubscriber(sub, verifiedEvent)
      }
    }

    return verifiedEvent.id
  }

  private async signEvent(
    event: UnsignedEvent,
    signerSecretKey?: Uint8Array
  ): Promise<VerifiedEvent> {
    this.eventCounter++

    // If event is already signed, just return it
    if ('id' in event && 'sig' in event && event.id && (event as VerifiedEvent).sig) {
      return event as VerifiedEvent
    }

    const ndkEvent = new NDKEvent()
    ndkEvent.kind = event.kind
    ndkEvent.content = event.content
    ndkEvent.tags = event.tags || []
    ndkEvent.created_at = event.created_at
    ndkEvent.pubkey = event.pubkey

    if (signerSecretKey) {
      const signer = new NDKPrivateKeySigner(signerSecretKey)
      await ndkEvent.sign(signer)
    }

    return {
      ...event,
      id: ndkEvent.id!,
      sig: ndkEvent.sig!,
      tags: ndkEvent.tags || [],
    } as VerifiedEvent
  }

  // ============================================================================
  // Subscribing
  // ============================================================================

  /**
   * Subscribe to events matching the given filters.
   * Already-delivered events are replayed to new subscribers (matching original MockRelay behavior).
   */
  subscribe(
    filters: Filter | Filter[],
    onEvent: (event: VerifiedEvent) => void,
    onEose?: () => void
  ): SubscriptionHandle {
    this.subscriptionCounter++
    const subId = `sub-${this.subscriptionCounter}`
    const filterArray = Array.isArray(filters) ? filters : [filters]

    const subscription: Subscription = {
      id: subId,
      filters: filterArray,
      onEvent,
      onEose,
      createdAt: Date.now(),
      delivered: new Set(),
      eoseSent: false,
      closed: false,
    }

    this.subscriptions.set(subId, subscription)
    this.log(`Subscription ${subId} created with ${filterArray.length} filter(s)`)

    // Replay delivered events to new subscribers
    // If cascadeReplay is enabled, also replay to any new subscriptions created during processing
    queueMicrotask(() => {
      if (this.cascadeReplay) {
        this.replayWithCascade()
      } else {
        for (const event of this.deliveredEvents) {
          this.deliverEventToSubscriber(subscription, event)
        }
      }
      if (this.autoEose) {
        this.sendEose(subId)
      }
    })

    return {
      id: subId,
      close: () => {
        subscription.closed = true
        this.subscriptions.delete(subId)
        this.log(`Subscription ${subId} closed`)
      },
    }
  }

  // ============================================================================
  // Delivery Control
  // ============================================================================

  /**
   * Deliver the next pending event (FIFO order) to all matching subscribers.
   * @returns true if an event was delivered, false if queue is empty
   */
  deliverNext(): boolean {
    const pending = this.pendingEvents.shift()
    if (!pending) {
      this.log("deliverNext: no pending events")
      return false
    }

    this.deliverPendingEvent(pending)
    return true
  }

  /**
   * Deliver all pending events in FIFO order.
   */
  deliverAll(): void {
    this.log(`deliverAll: delivering ${this.pendingEvents.length} events`)
    while (this.pendingEvents.length > 0) {
      this.deliverNext()
    }
  }

  /**
   * Deliver a specific event by ID.
   * @returns true if the event was found and delivered
   */
  deliverEvent(eventId: string): boolean {
    const index = this.pendingEvents.findIndex((p) => p.event.id === eventId)
    if (index === -1) {
      this.log(`deliverEvent: event ${eventId} not found in pending queue`)
      return false
    }

    const [pending] = this.pendingEvents.splice(index, 1)
    this.deliverPendingEvent(pending)
    return true
  }

  /**
   * Deliver events in a specific order (for out-of-order testing).
   * Events not in the list remain in the pending queue.
   */
  deliverInOrder(eventIds: string[]): void {
    this.log(`deliverInOrder: delivering ${eventIds.length} events`)
    for (const eventId of eventIds) {
      this.deliverEvent(eventId)
    }
  }

  /**
   * Deliver a specific event to a specific subscriber only.
   * @returns true if both event and subscriber were found
   */
  deliverTo(subscriberId: string, eventId: string): boolean {
    const sub = this.subscriptions.get(subscriberId)
    if (!sub || sub.closed) {
      this.log(`deliverTo: subscriber ${subscriberId} not found or closed`)
      return false
    }

    // Check pending events first
    const pendingIndex = this.pendingEvents.findIndex((p) => p.event.id === eventId)
    if (pendingIndex !== -1) {
      const pending = this.pendingEvents[pendingIndex]
      this.deliverEventToSubscriber(sub, pending.event)

      // Mark as delivered to this subscriber in pending event
      if (!pending.targetSubscribers) {
        pending.targetSubscribers = []
      }
      // Don't remove from pending - other subscribers may need it
      return true
    }

    // Check already delivered events
    const delivered = this.deliveredEvents.find((e) => e.id === eventId)
    if (delivered) {
      this.deliverEventToSubscriber(sub, delivered)
      return true
    }

    this.log(`deliverTo: event ${eventId} not found`)
    return false
  }

  /**
   * Deliver all pending events to a specific subscriber only.
   */
  deliverAllTo(subscriberId: string): void {
    const sub = this.subscriptions.get(subscriberId)
    if (!sub || sub.closed) {
      this.log(`deliverAllTo: subscriber ${subscriberId} not found or closed`)
      return
    }

    for (const pending of this.pendingEvents) {
      this.deliverEventToSubscriber(sub, pending.event)
    }
  }

  private deliverPendingEvent(pending: PendingEvent): void {
    const { event, targetSubscribers } = pending

    this.deliveredEvents.push(event)
    this.log(`Delivering event ${event.id} to subscribers`)

    for (const sub of this.subscriptions.values()) {
      if (sub.closed) continue
      if (targetSubscribers && !targetSubscribers.includes(sub.id)) continue

      this.deliverEventToSubscriber(sub, event)
    }
  }

  private deliverEventToSubscriber(sub: Subscription, event: VerifiedEvent): void {
    // Check if already delivered to this subscriber
    if (sub.delivered.has(event.id)) {
      console.warn(`[ControlledMockRelay] Event ${event.id?.slice(0, 8)} already delivered to ${sub.id}, skipping`)
      return
    }

    // Check if event matches any of the subscription's filters
    const matches = sub.filters.some((filter) => matchFilter(filter, event))
    if (!matches) {
      const filterInfo = sub.filters.map(f => `authors:${f.authors?.map(a => a.slice(0, 8)).join(',') || 'none'}, kinds:${f.kinds?.join(',') || 'any'}`).join('; ')
      console.warn(`[ControlledMockRelay] Event (pubkey=${event.pubkey?.slice(0, 8)}, kind=${event.kind}) doesn't match ${sub.id} (${filterInfo})`)
      return
    }

    sub.delivered.add(event.id)

    this.deliveryHistory.push({
      eventId: event.id,
      subscriberId: sub.id,
      timestamp: Date.now(),
      filters: sub.filters,
    })

    console.warn(`[ControlledMockRelay] Event (pubkey=${event.pubkey?.slice(0, 8)}, kind=${event.kind}) DELIVERED to ${sub.id}`)
    sub.onEvent(event)
  }

  // ============================================================================
  // Timing Control
  // ============================================================================

  /**
   * Deliver the next pending event after a delay.
   */
  async deliverNextAfter(delayMs: number): Promise<boolean> {
    await this.delay(delayMs)
    return this.deliverNext()
  }

  /**
   * Deliver all pending events with random jitter between each.
   */
  async deliverWithJitter(minMs: number, maxMs: number): Promise<void> {
    while (this.pendingEvents.length > 0) {
      const jitter = Math.random() * (maxMs - minMs) + minMs
      await this.delay(jitter)
      this.deliverNext()
    }
  }

  /**
   * Deliver all pending events with a fixed delay between each.
   */
  async deliverAllWithDelay(delayMs: number): Promise<void> {
    while (this.pendingEvents.length > 0) {
      await this.delay(delayMs)
      this.deliverNext()
    }
  }

  private delay(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms))
  }

  /**
   * Replay events with cascade - keeps replaying to new subscriptions
   * that are created during event processing (e.g., when Session receives
   * a message and subscribes to the sender's new key).
   */
  private replayWithCascade(maxIterations = 20): void {
    const replayedTo = new Set<string>()
    let iteration = 0

    while (iteration < maxIterations) {
      // Find subscriptions that haven't been replayed to yet
      const newSubs = Array.from(this.subscriptions.values())
        .filter(s => !s.closed && !replayedTo.has(s.id))

      if (newSubs.length === 0) break

      console.warn(`[ControlledMockRelay] replayWithCascade: iteration ${iteration}, replaying to ${newSubs.length} new sub(s): ${newSubs.map(s => s.id).join(', ')}`)

      for (const sub of newSubs) {
        replayedTo.add(sub.id)
        for (const event of this.deliveredEvents) {
          this.deliverEventToSubscriber(sub, event)
        }
      }

      iteration++
    }

    if (iteration >= maxIterations) {
      console.warn(`[ControlledMockRelay] replayWithCascade: hit max iterations (${maxIterations})`)
    }
  }

  // ============================================================================
  // Failure Injection
  // ============================================================================

  /**
   * Drop an event from the pending queue without delivering it.
   * @returns true if the event was found and dropped
   */
  dropEvent(eventId: string): boolean {
    const index = this.pendingEvents.findIndex((p) => p.event.id === eventId)
    if (index === -1) {
      this.log(`dropEvent: event ${eventId} not found`)
      return false
    }

    this.pendingEvents.splice(index, 1)
    this.log(`dropEvent: dropped event ${eventId}`)
    return true
  }

  /**
   * Drop the next N events from the pending queue.
   */
  dropNext(count: number = 1): void {
    const dropped = this.pendingEvents.splice(0, count)
    this.log(`dropNext: dropped ${dropped.length} events`)
  }

  /**
   * Duplicate an event - deliver it again to all matching subscribers.
   * Works on both pending and already-delivered events.
   * Note: This bypasses the normal deduplication.
   */
  duplicateEvent(eventId: string): boolean {
    // Find in pending
    const pending = this.pendingEvents.find((p) => p.event.id === eventId)
    if (pending) {
      this.forceDuplicateDelivery(pending.event)
      return true
    }

    // Find in delivered
    const delivered = this.deliveredEvents.find((e) => e.id === eventId)
    if (delivered) {
      this.forceDuplicateDelivery(delivered)
      return true
    }

    this.log(`duplicateEvent: event ${eventId} not found`)
    return false
  }

  private forceDuplicateDelivery(event: VerifiedEvent): void {
    this.log(`duplicateEvent: force-delivering ${event.id} again`)

    for (const sub of this.subscriptions.values()) {
      if (sub.closed) continue

      const matches = sub.filters.some((filter) => matchFilter(filter, event))
      if (!matches) continue

      // Record in history even though it's a duplicate
      this.deliveryHistory.push({
        eventId: event.id,
        subscriberId: sub.id,
        timestamp: Date.now(),
        filters: sub.filters,
      })

      // Call handler without checking delivered set
      sub.onEvent(event)
    }
  }

  /**
   * Simulate a disconnect - closes all subscriptions.
   * @param clearPending If true, also clears the pending event queue
   */
  simulateDisconnect(clearPending: boolean = false): void {
    this.log(`simulateDisconnect: closing ${this.subscriptions.size} subscriptions`)

    for (const sub of this.subscriptions.values()) {
      sub.closed = true
    }
    this.subscriptions.clear()

    if (clearPending) {
      this.pendingEvents = []
      this.log("simulateDisconnect: cleared pending events")
    }
  }

  /**
   * Simulate a reconnect - re-runs all active subscriptions against stored events.
   * Note: You need to re-subscribe after calling simulateDisconnect.
   */
  simulateReconnect(): void {
    this.log("simulateReconnect: replaying events to subscribers")

    for (const sub of this.subscriptions.values()) {
      if (sub.closed) continue

      // Clear delivered set to allow re-delivery
      sub.delivered.clear()
      sub.eoseSent = false

      // Replay all stored events
      for (const event of this.deliveredEvents) {
        this.deliverEventToSubscriber(sub, event)
      }

      // Deliver any pending events
      for (const pending of this.pendingEvents) {
        this.deliverEventToSubscriber(sub, pending.event)
      }

      if (this.autoEose) {
        this.sendEose(sub.id)
      }
    }
  }

  // ============================================================================
  // EOSE Control
  // ============================================================================

  /**
   * Send EOSE (End of Stored Events) to a specific subscription.
   */
  sendEose(subscriberId: string): boolean {
    const sub = this.subscriptions.get(subscriberId)
    if (!sub || sub.closed) {
      this.log(`sendEose: subscriber ${subscriberId} not found or closed`)
      return false
    }

    if (sub.eoseSent) {
      this.log(`sendEose: EOSE already sent to ${subscriberId}`)
      return false
    }

    sub.eoseSent = true
    this.log(`sendEose: sending EOSE to ${subscriberId}`)

    if (sub.onEose) {
      sub.onEose()
    }

    return true
  }

  /**
   * Send EOSE to all active subscriptions.
   */
  sendEoseToAll(): void {
    this.log(`sendEoseToAll: sending EOSE to ${this.subscriptions.size} subscriptions`)

    for (const subId of this.subscriptions.keys()) {
      this.sendEose(subId)
    }
  }

  /**
   * Enable or disable auto-EOSE mode.
   * When enabled, EOSE is automatically sent after delivering stored events to new subscriptions.
   */
  setAutoEose(enabled: boolean): void {
    this.autoEose = enabled
    this.log(`setAutoEose: ${enabled ? "enabled" : "disabled"}`)
  }

  // ============================================================================
  // Inspection
  // ============================================================================

  /**
   * Get all pending events (not yet delivered).
   */
  getPendingEvents(): VerifiedEvent[] {
    return this.pendingEvents.map((p) => p.event)
  }

  /**
   * Get the number of pending events.
   */
  getPendingCount(): number {
    return this.pendingEvents.length
  }

  /**
   * Get all events (both delivered and pending).
   */
  getAllEvents(): VerifiedEvent[] {
    const pending = this.pendingEvents.map((p) => p.event)
    return [...this.deliveredEvents, ...pending]
  }

  /**
   * Get all active subscriptions.
   */
  getSubscriptions(): SubscriptionInfo[] {
    return Array.from(this.subscriptions.values())
      .filter((s) => !s.closed)
      .map((s) => ({
        id: s.id,
        filters: s.filters,
        createdAt: s.createdAt,
        eoseSent: s.eoseSent,
      }))
  }

  /**
   * Check if an event was delivered to a specific subscriber.
   */
  wasDeliveredTo(eventId: string, subscriberId: string): boolean {
    return this.deliveryHistory.some(
      (r) => r.eventId === eventId && r.subscriberId === subscriberId
    )
  }

  /**
   * Get the full delivery history.
   */
  getDeliveryHistory(): DeliveryRecord[] {
    return [...this.deliveryHistory]
  }

  /**
   * Get delivery count for a specific event.
   */
  getDeliveryCount(eventId: string): number {
    return this.deliveryHistory.filter((r) => r.eventId === eventId).length
  }

  // ============================================================================
  // Reset
  // ============================================================================

  /**
   * Clear everything and reset to initial state.
   */
  reset(): void {
    this.pendingEvents = []
    this.deliveredEvents = []
    this.subscriptions.clear()
    this.deliveryHistory = []
    this.subscriptionCounter = 0
    this.eventCounter = 0
    this.autoEose = false
    this.log("reset: cleared all state")
  }

  /**
   * Clear only pending events.
   */
  clearPending(): void {
    const count = this.pendingEvents.length
    this.pendingEvents = []
    this.log(`clearPending: cleared ${count} pending events`)
  }

  /**
   * Clear delivery history.
   */
  clearHistory(): void {
    const count = this.deliveryHistory.length
    this.deliveryHistory = []
    this.log(`clearHistory: cleared ${count} records`)
  }

  // ============================================================================
  // Debug
  // ============================================================================

  private log(message: string): void {
    if (this.debug) {
      console.log(`[ControlledMockRelay] ${message}`)
    }
  }

  /**
   * Enable or disable debug logging.
   */
  setDebug(enabled: boolean): void {
    this.debug = enabled
  }
}
