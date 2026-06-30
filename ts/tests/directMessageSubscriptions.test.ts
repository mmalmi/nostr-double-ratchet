import { describe, expect, it } from "vitest"

import {
  buildInviteResponseBackfillFilter,
  buildDirectMessageBackfillFilter,
  buildRuntimeBackfillFilters,
  directMessageSubscriptionAuthors,
  DirectMessageSubscriptionTracker,
  inviteResponseSubscriptionRecipients,
  RuntimeSubscriptionTracker,
} from "../src/directMessageSubscriptions"

describe("direct message subscription helpers", () => {
  it("normalizes direct message authors and ignores invalid filters", () => {
    expect(
      directMessageSubscriptionAuthors({
        kinds: [1060],
        authors: [
          "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "nope",
        ],
      })
    ).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    expect(
      directMessageSubscriptionAuthors({
        kinds: [1],
        authors: ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
      })
    ).toEqual([])
  })

  it("tracks only newly added authors across overlapping subscriptions", () => {
    const tracker = new DirectMessageSubscriptionTracker()

    const first = tracker.registerFilter({
      kinds: [1060],
      authors: [
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      ],
    })
    expect(first.addedAuthors).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    const second = tracker.registerFilter({
      kinds: [1060],
      authors: ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
    })
    expect(second.addedAuthors).toEqual([])
    expect(tracker.trackedAuthors()).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    tracker.unregister(first.token)
    expect(tracker.trackedAuthors()).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    tracker.unregister(second.token)
    expect(tracker.trackedAuthors()).toEqual([])
  })

  it("builds a normalized direct message backfill filter", () => {
    expect(
      buildDirectMessageBackfillFilter(
        [
          "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ],
        1234,
        50
      )
    ).toEqual({
      kinds: [1060],
      authors: [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      ],
      since: 1234,
      limit: 50,
    })
  })

  it("normalizes invite response recipients", () => {
    expect(
      inviteResponseSubscriptionRecipients({
        kinds: [1059],
        "#p": [
          "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "nope",
        ],
      })
    ).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    expect(
      inviteResponseSubscriptionRecipients({
        kinds: [1060],
        "#p": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
      })
    ).toEqual([])
  })

  it("builds a normalized invite response backfill filter", () => {
    expect(
      buildInviteResponseBackfillFilter(
        [
          "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ],
        1234,
        50
      )
    ).toEqual({
      kinds: [1059],
      "#p": [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      ],
      since: 1234,
      limit: 50,
    })
  })

  it("tracks runtime message authors and invite response recipients", () => {
    const tracker = new RuntimeSubscriptionTracker()

    const first = tracker.registerFilter({
      kinds: [1060],
      authors: ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
    })
    expect(first.addedMessageAuthors).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])
    expect(first.addedInviteResponseRecipients).toEqual([])

    const second = tracker.registerFilter({
      kinds: [1059],
      "#p": ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
    })
    expect(second.addedMessageAuthors).toEqual([])
    expect(second.addedInviteResponseRecipients).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])
    expect(tracker.trackedMessageAuthors()).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])
    expect(tracker.trackedInviteResponseRecipients()).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    const third = tracker.registerFilter({
      kinds: [1059],
      "#p": ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
    })
    expect(third.addedInviteResponseRecipients).toEqual([])

    tracker.unregister(second.token)
    expect(tracker.trackedInviteResponseRecipients()).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])
    tracker.unregister(third.token)
    expect(tracker.trackedInviteResponseRecipients()).toEqual([])
  })

  it("builds runtime backfill filters for new subscription targets", () => {
    expect(
      buildRuntimeBackfillFilters(
        {
          addedMessageAuthors: [
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          ],
          addedInviteResponseRecipients: [
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          ],
        },
        1234,
        50
      )
    ).toEqual([
      {
        kinds: [1060],
        authors: ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        since: 1234,
        limit: 50,
      },
      {
        kinds: [1059],
        "#p": ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
        since: 1234,
        limit: 50,
      },
    ])
  })
})
