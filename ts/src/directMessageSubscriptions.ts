import type { Filter } from "nostr-tools"

import { MESSAGE_EVENT_KIND } from "./types"

export interface RegisteredDirectMessageSubscription {
  token: number
  addedAuthors: string[]
}

const normalizeAuthor = (value: unknown): string | null => {
  if (typeof value !== "string") return null
  const normalized = value.trim().toLowerCase()
  if (!/^[0-9a-f]{64}$/.test(normalized)) return null
  return normalized
}

export function directMessageSubscriptionAuthors(filter: Filter): string[] {
  if (!Array.isArray(filter.kinds) || !filter.kinds.includes(MESSAGE_EVENT_KIND)) {
    return []
  }

  if (!Array.isArray(filter.authors)) {
    return []
  }

  const authors: string[] = []
  const seen = new Set<string>()
  for (const author of filter.authors) {
    const normalized = normalizeAuthor(author)
    if (!normalized || seen.has(normalized)) continue
    seen.add(normalized)
    authors.push(normalized)
  }
  return authors
}

export function buildDirectMessageBackfillFilter(
  authors: Iterable<string>,
  since: number,
  limit: number = 200
): Filter {
  const normalizedAuthors: string[] = []
  const seen = new Set<string>()
  for (const author of authors) {
    const normalized = normalizeAuthor(author)
    if (!normalized || seen.has(normalized)) continue
    seen.add(normalized)
    normalizedAuthors.push(normalized)
  }

  return {
    kinds: [MESSAGE_EVENT_KIND],
    authors: normalizedAuthors,
    since,
    limit,
  }
}

export class DirectMessageSubscriptionTracker {
  private nextToken = 1
  private authorsByToken = new Map<number, string[]>()
  private authorRefCounts = new Map<string, number>()

  registerFilter(filter: Filter): RegisteredDirectMessageSubscription {
    const token = this.nextToken++
    const authors = directMessageSubscriptionAuthors(filter)
    if (authors.length === 0) {
      return { token, addedAuthors: [] }
    }

    this.authorsByToken.set(token, authors)
    const addedAuthors: string[] = []
    for (const author of authors) {
      const refCount = this.authorRefCounts.get(author) || 0
      if (refCount === 0) {
        addedAuthors.push(author)
      }
      this.authorRefCounts.set(author, refCount + 1)
    }

    return { token, addedAuthors }
  }

  unregister(token: number): void {
    const authors = this.authorsByToken.get(token)
    if (!authors) return

    this.authorsByToken.delete(token)
    for (const author of authors) {
      const nextCount = Math.max((this.authorRefCounts.get(author) || 1) - 1, 0)
      if (nextCount === 0) {
        this.authorRefCounts.delete(author)
      } else {
        this.authorRefCounts.set(author, nextCount)
      }
    }
  }

  trackedAuthors(): string[] {
    return Array.from(this.authorRefCounts.keys()).sort()
  }
}
