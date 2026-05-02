import { describe, expect, it } from "vitest"
import { GROUP_METADATA_KIND } from "../src/GroupMeta"
import {
  CHAT_MESSAGE_KIND,
  CHAT_SETTINGS_KIND,
  EXPIRATION_TAG,
  type Rumor,
} from "../src/types"
import {
  applyExpirationPolicy,
  chatSettingsAdoptionForRumor,
  expirationOverrideFromSendOptions,
} from "../src/session-manager/messagePolicy"

describe("session-manager messagePolicy", () => {
  it("applies group, peer, default and per-send expiration precedence", () => {
    const tags = [["p", "bob"], ["l", "group-a"]]

    applyExpirationPolicy({
      kind: CHAT_MESSAGE_KIND,
      nowSeconds: 1_700_000_000,
      tags,
      expirationOverride: expirationOverrideFromSendOptions({}),
      defaultExpiration: { ttlSeconds: 60 },
      peerExpiration: { ttlSeconds: 30 },
      hasPeerExpiration: true,
      groupExpiration: { ttlSeconds: 10 },
      hasGroupExpiration: true,
    })

    expect(tags).toContainEqual([EXPIRATION_TAG, "1700000010"])

    applyExpirationPolicy({
      kind: CHAT_MESSAGE_KIND,
      nowSeconds: 1_700_000_000,
      tags,
      expirationOverride: { ttlSeconds: 5 },
      defaultExpiration: { ttlSeconds: 60 },
      peerExpiration: { ttlSeconds: 30 },
      hasPeerExpiration: true,
      groupExpiration: { ttlSeconds: 10 },
      hasGroupExpiration: true,
    })

    expect(tags.filter(([name]) => name === EXPIRATION_TAG)).toEqual([
      [EXPIRATION_TAG, "1700000005"],
    ])
  })

  it("keeps excluded kinds and disabled policies unexpired", () => {
    for (const kind of [GROUP_METADATA_KIND, CHAT_SETTINGS_KIND]) {
      const tags: string[][] = []
      applyExpirationPolicy({
        kind,
        nowSeconds: 1_700_000_000,
        tags,
        expirationOverride: { ttlSeconds: 5 },
        hasPeerExpiration: false,
        hasGroupExpiration: false,
      })
      expect(tags).toEqual([])
    }

    const tags: string[][] = []
    applyExpirationPolicy({
      kind: CHAT_MESSAGE_KIND,
      nowSeconds: 1_700_000_000,
      tags,
      expirationOverride: null,
      defaultExpiration: { ttlSeconds: 60 },
      hasPeerExpiration: false,
      hasGroupExpiration: false,
    })
    expect(tags).toEqual([])
  })

  it("derives chat-settings adoption from incoming and sender-copy rumors", () => {
    const incoming = rumor({
      content: JSON.stringify({
        type: "chat-settings",
        v: 1,
        messageTtlSeconds: 90,
      }),
      tags: [["p", "alice"]],
    })
    expect(chatSettingsAdoptionForRumor(incoming, "bob", "alice")).toEqual({
      peerPubkey: "bob",
      options: { ttlSeconds: 90 },
    })

    const senderCopy = rumor({
      content: JSON.stringify({
        type: "chat-settings",
        v: 1,
        messageTtlSeconds: null,
      }),
      tags: [["p", "bob"]],
    })
    expect(chatSettingsAdoptionForRumor(senderCopy, "alice", "alice")).toEqual({
      peerPubkey: "bob",
      options: null,
    })
  })
})

function rumor(overrides: Partial<Rumor>): Rumor {
  return {
    content: "",
    created_at: 0,
    id: "rumor-id",
    kind: CHAT_SETTINGS_KIND,
    pubkey: "alice",
    tags: [],
    ...overrides,
  }
}
