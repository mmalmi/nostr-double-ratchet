import { Filter, VerifiedEvent } from "nostr-tools"
import { matchFilter } from "nostr-tools"

/**
 * Check if a Nostr event kind is replaceable (only the latest event per key should be kept).
 */
function isReplaceableKind(kind: number): boolean {
  return kind === 0 || kind === 3 || (kind >= 10000 && kind < 20000) || (kind >= 30000 && kind < 40000)
}

function getReplaceableKey(event: VerifiedEvent): string {
  if (event.kind >= 30000 && event.kind < 40000) {
    const dTag = event.tags?.find((t: string[]) => t[0] === "d")?.[1] || ""
    return `${event.kind}:${event.pubkey}:${dTag}`
  }
  return `${event.kind}:${event.pubkey}`
}

function deduplicateReplaceable(events: VerifiedEvent[]): VerifiedEvent[] {
  const latestByKey = new Map<string, VerifiedEvent>()
  const nonReplaceable: VerifiedEvent[] = []

  for (const event of events) {
    if (isReplaceableKind(event.kind)) {
      const key = getReplaceableKey(event)
      const existing = latestByKey.get(key)
      if (!existing || event.created_at >= existing.created_at) {
        latestByKey.set(key, event)
      }
    } else {
      nonReplaceable.push(event)
    }
  }

  return [...nonReplaceable, ...latestByKey.values()]
}

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

    // Deliver existing matching events.
    // For replaceable events, only deliver the latest per author+d-tag (Nostr relay behavior).
    const existingMatches = deduplicateReplaceable(
      this.events.filter((event) => matchFilter(filter, event))
    )
    for (const event of existingMatches) {
      onEvent(event)
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
