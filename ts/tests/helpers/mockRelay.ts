import { matchFilter, VerifiedEvent, UnsignedEvent, Filter } from "nostr-tools"
import { NDKEvent, NDKPrivateKeySigner } from "@nostr-dev-kit/ndk"

type Subscriber = {
  id: string
  filter: Filter
  onEvent: (e: VerifiedEvent) => void
  delivered: Set<string>
}

export class MockRelay {
  private events: VerifiedEvent[] = []
  private subscribers: Map<string, Subscriber> = new Map()
  private subscriptionCounter = 0
  private debug: boolean = false

  constructor(debug: boolean = false) {
    this.debug = debug
  }

  getEvents(): VerifiedEvent[] {
    return [...this.events]
  }

  getSubscriptions(): Map<string, Subscriber> {
    return new Map(this.subscribers)
  }

  async publish(
    event: UnsignedEvent,
    signerSecretKey?: Uint8Array
  ): Promise<VerifiedEvent> {
    // Check if event is already signed (has id and sig)
    const isAlreadySigned = 'id' in event && 'sig' in event && event.id && (event as VerifiedEvent).sig

    let verifiedEvent: VerifiedEvent

    if (isAlreadySigned) {
      // Event is already signed, use it as-is
      verifiedEvent = event as VerifiedEvent
    } else {
      // Event needs signing
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

      verifiedEvent = {
        ...event,
        pubkey: ndkEvent.pubkey,
        id: ndkEvent.id!,
        sig: ndkEvent.sig!,
        tags: ndkEvent.tags || [],
      } as VerifiedEvent
    }

    // Handle replaceable events (kinds 10000-19999) and parameterized replaceable events (30000-39999)
    // Keep only latest per pubkey + kind + d-tag
    const isReplaceable = (event.kind >= 10000 && event.kind < 20000) ||
                          (event.kind >= 30000 && event.kind < 40000)
    if (isReplaceable) {
      const dTag = event.tags?.find((t) => t[0] === "d")?.[1]
      this.events = this.events.filter((e) => {
        if (e.kind !== event.kind || e.pubkey !== verifiedEvent.pubkey) return true
        const existingDTag = e.tags?.find((t) => t[0] === "d")?.[1]
        return existingDTag !== dTag
      })
    }

    this.events.push(verifiedEvent)

    if (this.debug) {
      const pTags = verifiedEvent.tags?.filter(t => t[0] === 'p').map(t => t[1]?.slice(0,8)).join(',') || 'none'
      console.log(`[MockRelay] PUBLISH: event(pubkey=${verifiedEvent.pubkey?.slice(0,8)}, kind=${verifiedEvent.kind}, p-tags=${pTags}) to ${this.subscribers.size} subscribers`)
    }

    for (const sub of this.subscribers.values()) {
      this.deliverToSubscriber(sub, verifiedEvent)
    }

    return verifiedEvent
  }

  subscribe(filter: Filter, onEvent: (event: VerifiedEvent) => void): () => void {
    this.subscriptionCounter++
    const subId = `sub-${this.subscriptionCounter}`

    const subscriber: Subscriber = {
      id: subId,
      filter,
      onEvent,
      delivered: new Set(),
    }

    this.subscribers.set(subId, subscriber)

    if (this.debug) {
      const pTags = (filter as Record<string, unknown>)['#p'] as string[] | undefined
      console.log(`[MockRelay] SUBSCRIBE: ${subId} filter(authors=${filter.authors?.map(a => a.slice(0,8)).join(',') || 'none'}, kinds=${filter.kinds?.join(',') || 'any'}, #p=${pTags?.map(p => p.slice(0,8)).join(',') || 'none'})`)
      console.log(`[MockRelay] Replaying ${this.events.length} existing events to ${subId}`)
    }

    // Defer initial delivery to next tick to allow subscriber assignment to complete
    // This mimics real relay behavior where events arrive asynchronously
    queueMicrotask(() => {
      for (const event of this.events) {
        this.deliverToSubscriber(subscriber, event)
      }
    })

    return () => {
      this.subscribers.delete(subId)
    }
  }

  private deliverToSubscriber(subscriber: Subscriber, event: VerifiedEvent): void {
    if (subscriber.delivered.has(event.id)) {
      if (this.debug) {
        console.log(`[MockRelay] ALREADY DELIVERED: event(id=${event.id?.slice(0,8)}) to ${subscriber.id}`)
      }
      return
    }
    const matches = matchFilter(subscriber.filter, event)
    if (!matches) {
      // Log filter mismatches for debugging (only for kind 1059 to reduce noise)
      if (this.debug && event.kind === 1059) {
        const pTags = (subscriber.filter as Record<string, unknown>)['#p'] as string[] | undefined
        const eventPTags = event.tags?.filter(t => t[0] === 'p').map(t => t[1]?.slice(0,8)).join(',') || 'none'
        console.log(`[MockRelay] NO MATCH 1059: event(pubkey=${event.pubkey?.slice(0,8)}, p-tags=${eventPTags}) vs ${subscriber.id}(#p=${pTags?.map(p => p.slice(0,8)).join(',') || 'none'})`)
      }
      return
    }
    if (this.debug) {
      console.log(`[MockRelay] MATCH: event(pubkey=${event.pubkey?.slice(0,8)}, kind=${event.kind}) delivered to ${subscriber.id}`)
    }
    subscriber.delivered.add(event.id)
    try {
      subscriber.onEvent(event)
    } catch (error) {
      if (this.shouldIgnoreDecryptionError(error)) {
        console.warn("MockRelay: ignored decrypt error", error)
        return
      }
      throw error
    }
  }

  private shouldIgnoreDecryptionError(error: unknown): boolean {
    if (!(error instanceof Error)) return false
    const message = error.message?.toLowerCase()
    if (!message) return false
    return message.includes("invalid mac") || message.includes("failed to decrypt header")
  }

  clearEvents(): void {
    this.events = []
    for (const sub of this.subscribers.values()) {
      sub.delivered.clear()
    }
  }

  reset(): void {
    this.events = []
    this.subscribers.clear()
    this.subscriptionCounter = 0
  }
}
