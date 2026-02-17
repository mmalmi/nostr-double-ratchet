import { describe, expect, it } from "vitest";
import type { Filter, VerifiedEvent } from "nostr-tools";
import { generateSecretKey, getPublicKey } from "nostr-tools";

import {
  Group,
  GroupManager,
  GROUP_SENDER_KEY_DISTRIBUTION_KIND,
  type GroupData,
} from "../src/Group";
import { InMemoryStorageAdapter } from "../src/StorageAdapter";
import { REACTION_KIND } from "../src/types";
import type { NostrSubscribe, Rumor } from "../src/types";

function makeGroup(groupId: string, members: string[], admins: string[]): GroupData {
  return {
    id: groupId,
    name: "Test",
    members,
    admins,
    createdAt: Date.now(),
    accepted: true,
  };
}

describe("GroupManager", () => {
  it("drains queued outer events after sender-key distribution and emits callbacks", async () => {
    const groupId = "group-manager-queue";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());
    const aliceDevicePk = getPublicKey(generateSecretKey());
    const bobDevicePk = getPublicKey(generateSecretKey());

    const alice = new Group({
      data: makeGroup(groupId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]),
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      storage: new InMemoryStorageAdapter(),
    });

    const received: string[] = [];
    const filters: Filter[] = [];

    const manager = new GroupManager({
      ourOwnerPubkey: bobOwnerPk,
      ourDevicePubkey: bobDevicePk,
      storage: new InMemoryStorageAdapter(),
      nostrSubscribe: ((filter, _onEvent) => {
        filters.push(filter);
        return () => {};
      }) as NostrSubscribe,
      onDecryptedEvent: (event) => {
        received.push(event.inner.content);
      },
    });

    await manager.upsertGroup(makeGroup(groupId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]));

    let distribution: Rumor | null = null;
    let outer: VerifiedEvent | null = null;

    await alice.sendMessage("hello group", {
      sendPairwise: async (_to, rumor) => {
        distribution = rumor;
      },
      publishOuter: async (event) => {
        outer = event;
      },
    });

    expect(outer).not.toBeNull();
    expect(distribution).not.toBeNull();

    // Outer arrives before the manager has sender mapping.
    const early = await manager.handleOuterEvent(outer!);
    expect(early).toBeNull();
    expect(received).toEqual([]);

    const drained = await manager.handleIncomingSessionEvent(
      distribution!,
      aliceOwnerPk,
      aliceDevicePk
    );

    expect(drained).toHaveLength(1);
    expect(drained[0]!.inner.content).toBe("hello group");
    expect(received).toEqual(["hello group"]);

    // Manager should now subscribe to this sender-event author for future outers.
    const latestFilter = filters.at(-1);
    expect(latestFilter?.kinds).toEqual([outer!.kind]);
    expect(latestFilter?.authors).toEqual([outer!.pubkey]);
  });

  it("sendMessage uses device pubkey for inner rumor and sends distribution once", async () => {
    const groupId = "group-manager-send";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());
    const aliceDevicePk = getPublicKey(generateSecretKey());

    const manager = new GroupManager({
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      storage: new InMemoryStorageAdapter(),
    });

    await manager.upsertGroup(makeGroup(groupId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]));

    const pairwise: Rumor[] = [];
    const published: VerifiedEvent[] = [];

    const sent = await manager.sendMessage(groupId, "from-device", {
      sendPairwise: async (_to, rumor) => {
        pairwise.push(rumor);
      },
      publishOuter: async (outer) => {
        published.push(outer);
      },
    });

    expect(sent.inner.pubkey).toBe(aliceDevicePk);
    expect(pairwise).toHaveLength(1);
    expect(pairwise[0]!.kind).toBe(GROUP_SENDER_KEY_DISTRIBUTION_KIND);
    expect(pairwise[0]!.pubkey).toBe(aliceDevicePk);
    expect(published).toHaveLength(1);
  });

  it("sendEvent preserves inner kind and tags for non-message group events", async () => {
    const groupId = "group-manager-send-event";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());
    const aliceDevicePk = getPublicKey(generateSecretKey());

    const manager = new GroupManager({
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      storage: new InMemoryStorageAdapter(),
    });

    await manager.upsertGroup(makeGroup(groupId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]));

    const sent = await manager.sendEvent(
      groupId,
      {
        kind: REACTION_KIND,
        content: "👍",
        tags: [["e", "target-event-id"]],
      },
      {
        sendPairwise: async () => {},
        publishOuter: async () => {},
      }
    );

    expect(sent.inner.kind).toBe(REACTION_KIND);
    expect(sent.inner.pubkey).toBe(aliceDevicePk);
    expect(sent.inner.tags.some((tag) => tag[0] === "e" && tag[1] === "target-event-id")).toBe(true);
    expect(sent.inner.tags.some((tag) => tag[0] === "l" && tag[1] === groupId)).toBe(true);
  });

  it("re-subscribes outer authors when new sender-event pubkeys are learned", async () => {
    const groupAId = "group-a";
    const groupBId = "group-b";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());
    const aliceDevicePk = getPublicKey(generateSecretKey());

    const senderForGroupA = new Group({
      data: makeGroup(groupAId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]),
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      storage: new InMemoryStorageAdapter(),
    });

    const senderForGroupB = new Group({
      data: makeGroup(groupBId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]),
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      storage: new InMemoryStorageAdapter(),
    });

    const filters: Filter[] = [];
    let unsubscribeCalls = 0;

    const manager = new GroupManager({
      ourOwnerPubkey: bobOwnerPk,
      ourDevicePubkey: getPublicKey(generateSecretKey()),
      storage: new InMemoryStorageAdapter(),
      nostrSubscribe: ((filter, _onEvent) => {
        filters.push(filter);
        return () => {
          unsubscribeCalls += 1;
        };
      }) as NostrSubscribe,
    });

    await manager.upsertGroup(makeGroup(groupAId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]));
    await manager.upsertGroup(makeGroup(groupBId, [aliceOwnerPk, bobOwnerPk], [aliceOwnerPk]));

    let distA: Rumor | null = null;
    let outerA: VerifiedEvent | null = null;
    await senderForGroupA.sendMessage("a1", {
      sendPairwise: async (_to, rumor) => {
        distA = rumor;
      },
      publishOuter: async (outer) => {
        outerA = outer;
      },
    });

    let distB: Rumor | null = null;
    let outerB: VerifiedEvent | null = null;
    await senderForGroupB.sendMessage("b1", {
      sendPairwise: async (_to, rumor) => {
        distB = rumor;
      },
      publishOuter: async (outer) => {
        outerB = outer;
      },
    });

    await manager.handleIncomingSessionEvent(distA!, aliceOwnerPk, aliceDevicePk);
    expect(filters).toHaveLength(1);
    expect(filters[0]!.authors).toEqual([outerA!.pubkey]);
    expect(unsubscribeCalls).toBe(0);

    await manager.handleIncomingSessionEvent(distB!, aliceOwnerPk, aliceDevicePk);
    expect(filters).toHaveLength(2);
    expect(filters[1]!.authors).toEqual([outerA!.pubkey, outerB!.pubkey].sort());
    expect(unsubscribeCalls).toBe(1);
  });
});
