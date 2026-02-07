import { describe, expect, it } from "vitest";
import { getPublicKey, generateSecretKey } from "nostr-tools";

import { BroadcastChannel } from "../src/BroadcastChannel";
import { InMemoryStorageAdapter } from "../src/StorageAdapter";
import type { Rumor } from "../src/types";

describe("BroadcastChannel (OneToMany + sender keys)", () => {
  it("publishes one outer event and fan-outs one sender-key distribution per member", async () => {
    const groupId = "group-1";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());
    const carolOwnerPk = getPublicKey(generateSecretKey());

    const aliceDevicePk = getPublicKey(generateSecretKey());

    const alice = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk, carolOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const sent: Array<{ to: string; rumor: Rumor }> = [];
    const published: unknown[] = [];

    await alice.sendMessage("hello", {
      sendPairwise: async (to, rumor) => {
        sent.push({ to, rumor });
      },
      publishOuter: async (event) => {
        published.push(event);
      },
    });

    // One distribution per *other* member owner.
    expect(sent.map((s) => s.to).sort()).toEqual([bobOwnerPk, carolOwnerPk].sort());
    expect(published).toHaveLength(1);
  });

  it("decrypts a group message even if the outer event arrives before the sender-key distribution", async () => {
    const groupId = "group-2";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());

    const aliceDevicePk = getPublicKey(generateSecretKey());
    const bobDevicePk = getPublicKey(generateSecretKey());

    const alice = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const bob = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: bobOwnerPk,
      ourDevicePubkey: bobDevicePk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const sent: Array<{ to: string; rumor: Rumor }> = [];
    let outer: unknown | null = null;

    await alice.sendMessage("hello out of order", {
      sendPairwise: async (to, rumor) => {
        sent.push({ to, rumor });
      },
      publishOuter: async (event) => {
        outer = event;
      },
    });

    expect(outer).not.toBeNull();
    expect(sent).toHaveLength(1);

    // Deliver outer first (missing distribution) -> should queue/pending.
    const early = await bob.handleOuterEvent(outer as any);
    expect(early).toBeNull();

    // Now deliver the distribution via the 1:1 session path.
    const distRumor = sent[0]!.rumor;
    const after = await bob.handleIncomingSessionEvent(distRumor, aliceOwnerPk);
    expect(after).toHaveLength(1);
    expect(after[0]!.inner.content).toBe("hello out of order");
    expect(after[0]!.senderOwnerPubkey).toBe(aliceOwnerPk);
    expect(after[0]!.senderDevicePubkey).toBe(aliceDevicePk);
  });

  it("supports sender key rotation (new key id) and continues decrypting", async () => {
    const groupId = "group-3";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());

    const aliceDevicePk = getPublicKey(generateSecretKey());
    const bobDevicePk = getPublicKey(generateSecretKey());

    const alice = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const bob = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: bobOwnerPk,
      ourDevicePubkey: bobDevicePk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const sent: Array<{ to: string; rumor: Rumor }> = [];
    const published: unknown[] = [];

    // First message establishes initial key + distribution.
    await alice.sendMessage("m1", {
      sendPairwise: async (to, rumor) => sent.push({ to, rumor }),
      publishOuter: async (event) => published.push(event),
    });

    // Deliver distribution + outer.
    await bob.handleIncomingSessionEvent(sent.pop()!.rumor, aliceOwnerPk);
    const first = await bob.handleOuterEvent(published.pop() as any);
    expect(first?.inner.content).toBe("m1");
    const firstKeyId = first?.keyId;
    expect(typeof firstKeyId).toBe("number");

    // Rotate + send again.
    sent.length = 0;
    published.length = 0;
    await alice.rotateSenderKey({
      sendPairwise: async (to, rumor) => sent.push({ to, rumor }),
    });
    await alice.sendMessage("m2", {
      sendPairwise: async (to, rumor) => sent.push({ to, rumor }),
      publishOuter: async (event) => published.push(event),
    });

    // Deliver rotation distribution (there may be one for rotate + one for send; either order ok).
    for (const s of sent) {
      await bob.handleIncomingSessionEvent(s.rumor, aliceOwnerPk);
    }
    const second = await bob.handleOuterEvent(published[0] as any);
    expect(second?.inner.content).toBe("m2");
    expect(second?.keyId).not.toBe(firstKeyId);
  });

  it("multi-device: two devices for the same owner can decrypt after receiving the distribution", async () => {
    const groupId = "group-4";

    const aliceOwnerPk = getPublicKey(generateSecretKey());
    const bobOwnerPk = getPublicKey(generateSecretKey());

    const aliceDevicePk = getPublicKey(generateSecretKey());
    const bobDevice1Pk = getPublicKey(generateSecretKey());
    const bobDevice2Pk = getPublicKey(generateSecretKey());

    const alice = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: aliceOwnerPk,
      ourDevicePubkey: aliceDevicePk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const bob1 = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: bobOwnerPk,
      ourDevicePubkey: bobDevice1Pk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const bob2 = new BroadcastChannel({
      groupId,
      ourOwnerPubkey: bobOwnerPk,
      ourDevicePubkey: bobDevice2Pk,
      memberOwnerPubkeys: [aliceOwnerPk, bobOwnerPk],
      storage: new InMemoryStorageAdapter(),
    });

    const sent: Array<{ to: string; rumor: Rumor }> = [];
    let outer: unknown | null = null;

    await alice.sendMessage("hello bob devices", {
      sendPairwise: async (to, rumor) => sent.push({ to, rumor }),
      publishOuter: async (event) => {
        outer = event;
      },
    });

    // Deliver outer first to both devices.
    expect(await bob1.handleOuterEvent(outer as any)).toBeNull();
    expect(await bob2.handleOuterEvent(outer as any)).toBeNull();

    // Deliver the same distribution to both devices (simulating SessionManager fanout).
    expect(sent).toHaveLength(1);
    const distRumor = sent[0]!.rumor;
    const r1 = await bob1.handleIncomingSessionEvent(distRumor, aliceOwnerPk);
    const r2 = await bob2.handleIncomingSessionEvent(distRumor, aliceOwnerPk);

    expect(r1).toHaveLength(1);
    expect(r2).toHaveLength(1);
    expect(r1[0]!.inner.content).toBe("hello bob devices");
    expect(r2[0]!.inner.content).toBe("hello bob devices");
  });
});

