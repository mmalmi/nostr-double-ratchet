import { matchFilter, VerifiedEvent, UnsignedEvent } from 'nostr-tools'

// In-memory relay that supports replay to new subscribers and push-based
// delivery to all subscribers without relying on polling timeouts.

const relay: (UnsignedEvent & { sig?: string; id?: string })[] = []

type Subscriber = {
  filter: any
  onEvent: (e: VerifiedEvent) => void
  delivered: Set<object> // track events already sent to this subscriber
}

const subscribers: Subscriber[] = []

function deliverToSubscriber(sub: Subscriber, event: UnsignedEvent) {
  if (!sub.delivered.has(event) && matchFilter(sub.filter, event as any)) {
    sub.delivered.add(event)
    sub.onEvent(event as any)
  }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

export function publish(event: UnsignedEvent) {
  relay.push(event as any)

  // Push to existing subscribers immediately
  for (const sub of subscribers) {
    deliverToSubscriber(sub, event)
  }

  return Promise.resolve(event)
}

export function makeSubscribe() {
  return (filter: any, onEvent: (e: VerifiedEvent) => void) => {
    const subscriber: Subscriber = { filter, onEvent, delivered: new Set() }

    // Replay history
    for (const e of relay) {
      deliverToSubscriber(subscriber, e)
    }

    subscribers.push(subscriber)

    // Unsubscribe fn
    return () => {
      const idx = subscribers.indexOf(subscriber)
      if (idx !== -1) subscribers.splice(idx, 1)
    }
  }
} 