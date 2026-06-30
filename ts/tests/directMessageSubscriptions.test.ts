import { describe, expect, it } from "vitest"

import {
  appKeysSubscriptionAuthors,
  buildAppKeysBackfillFilter,
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

  it("normalizes app-keys authors", () => {
    expect(
      appKeysSubscriptionAuthors({
        kinds: [37368],
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
      appKeysSubscriptionAuthors({
        kinds: [7368],
        authors: ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
      })
    ).toEqual([])
  })

  it("builds a normalized app-keys backfill filter", () => {
    expect(
      buildAppKeysBackfillFilter(
        [
          "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ],
        1234,
        50
      )
    ).toEqual({
      kinds: [37368],
      authors: [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      ],
      since: 1234,
      limit: 50,
    })
  })

  it("tracks runtime app-keys authors, message authors, and invite response recipients", () => {
    const tracker = new RuntimeSubscriptionTracker()

    const first = tracker.registerFilter({
      kinds: [37368],
      authors: ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
    })
    expect(first.addedAppKeysAuthors).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])
    expect(first.addedMessageAuthors).toEqual([])
    expect(first.addedInviteResponseRecipients).toEqual([])

    const second = tracker.registerFilter({
      kinds: [1060],
      authors: ["cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],
    })
    expect(second.addedAppKeysAuthors).toEqual([])
    expect(second.addedMessageAuthors).toEqual([
      "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    ])
    expect(second.addedInviteResponseRecipients).toEqual([])

    const third = tracker.registerFilter({
      kinds: [1059],
      "#p": ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
    })
    expect(third.addedAppKeysAuthors).toEqual([])
    expect(third.addedMessageAuthors).toEqual([])
    expect(third.addedInviteResponseRecipients).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])
    expect(tracker.trackedAppKeysAuthors()).toEqual([
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])
    expect(tracker.trackedMessageAuthors()).toEqual([
      "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    ])
    expect(tracker.trackedInviteResponseRecipients()).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])

    const fourth = tracker.registerFilter({
      kinds: [1059],
      "#p": ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
    })
    expect(fourth.addedInviteResponseRecipients).toEqual([])

    tracker.unregister(third.token)
    expect(tracker.trackedInviteResponseRecipients()).toEqual([
      "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ])
    tracker.unregister(fourth.token)
    expect(tracker.trackedInviteResponseRecipients()).toEqual([])
  })

  it("builds runtime backfill filters for new subscription targets", () => {
    expect(
      buildRuntimeBackfillFilters(
        {
          addedAppKeysAuthors: [
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          ],
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
        kinds: [37368],
        authors: ["cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],
        since: 1234,
        limit: 50,
      },
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
