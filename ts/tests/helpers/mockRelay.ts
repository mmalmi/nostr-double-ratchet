import { Filter, VerifiedEvent } from "nostr-tools"
import { matchFilter } from "nostr-tools"

interface Subscription {
  id: string
  filter: Filter
  onEvent: (event: VerifiedEvent) => void
}

let subIdCounter = 0

export class MockRelay {
  private events: VerifiedEvent[] = []
  private subscriptions: Map<string, Subscription> = new Map()

  subscribe(filter: Filter, onEvent: (event: VerifiedEvent) => void): { id: string; close: () => void } {
    const id = `sub-${++subIdCounter}`
    const sub: Subscription = { id, filter, onEvent }
    this.subscriptions.set(id, sub)

    // Deliver existing matching events
    for (const event of this.events) {
      if (matchFilter(filter, event)) {
        onEvent(event)
      }
    }

    return {
      id,
      close: () => {
        this.subscriptions.delete(id)
      },
    }
  }

  storeAndDeliver(event: VerifiedEvent): void {
    this.events.push(event)
    for (const sub of this.subscriptions.values()) {
      if (matchFilter(sub.filter, event)) {
        sub.onEvent(event)
      }
    }
  }

  getAllEvents(): VerifiedEvent[] {
    return [...this.events]
  }

  clearEvents(): void {
    this.events = []
  }
}
