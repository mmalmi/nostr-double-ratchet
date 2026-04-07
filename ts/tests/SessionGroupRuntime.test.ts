import { describe, expect, it } from "vitest";
import { generateSecretKey, getPublicKey } from "nostr-tools";
import { SessionGroupRuntime } from "../src/RuntimeGroupController";
import type { OnEventCallback } from "../src/SessionManager";
import type { Rumor } from "../src/types";

class FakeSessionManager {
  readonly ownerPubkey: string;
  readonly devicePubkey: string;
  private readonly callbacks = new Set<OnEventCallback>();
  peer: FakeSessionManager | null = null;

  constructor(ownerPubkey: string, devicePubkey: string) {
    this.ownerPubkey = ownerPubkey;
    this.devicePubkey = devicePubkey;
  }

  onEvent(callback: OnEventCallback) {
    this.callbacks.add(callback);
    return () => {
      this.callbacks.delete(callback);
    };
  }

  async sendEvent(recipientOwnerPubkey: string, rumor: Rumor): Promise<Rumor> {
    if (this.peer && this.peer.ownerPubkey === recipientOwnerPubkey) {
      this.peer.emit(rumor, this.ownerPubkey, this.devicePubkey);
    }
    return rumor;
  }

  private emit(
    rumor: Rumor,
    senderOwnerPubkey: string,
    senderDevicePubkey: string,
  ): void {
    for (const callback of this.callbacks) {
      callback(rumor, senderOwnerPubkey, {
        senderOwnerPubkey,
        senderDevicePubkey,
      } as Parameters<OnEventCallback>[2]);
    }
  }
}

describe("SessionGroupRuntime", () => {
  it("attaches group transport to an existing SessionManager", async () => {
    const aliceOwnerPubkey = getPublicKey(generateSecretKey());
    const bobOwnerPubkey = getPublicKey(generateSecretKey());
    const aliceDevicePubkey = getPublicKey(generateSecretKey());
    const bobDevicePubkey = getPublicKey(generateSecretKey());

    const aliceSessionManager = new FakeSessionManager(
      aliceOwnerPubkey,
      aliceDevicePubkey,
    );
    const bobSessionManager = new FakeSessionManager(
      bobOwnerPubkey,
      bobDevicePubkey,
    );
    aliceSessionManager.peer = bobSessionManager;
    bobSessionManager.peer = aliceSessionManager;

    const aliceGroups = new SessionGroupRuntime({
      sessionManager: aliceSessionManager as never,
      ourOwnerPubkey: aliceOwnerPubkey,
      ourDevicePubkey: aliceDevicePubkey,
      nostrSubscribe: () => () => {},
      nostrPublish: async (event) => event as never,
    });
    const bobGroups = new SessionGroupRuntime({
      sessionManager: bobSessionManager as never,
      ourOwnerPubkey: bobOwnerPubkey,
      ourDevicePubkey: bobDevicePubkey,
      nostrSubscribe: () => () => {},
      nostrPublish: async (event) => event as never,
    });

    try {
      const created = await aliceGroups.createGroup(
        "SessionManager Group",
        [bobOwnerPubkey],
        {
          fanoutMetadata: false,
        },
      );
      await bobGroups.syncGroups([created.group]);

      const sent = await aliceGroups.sendGroupMessage(
        created.group.id,
        "hello from session runtime",
      );
      const decrypted = await bobGroups
        .getGroupManager()!
        .handleOuterEvent(sent.outer);

      expect(aliceGroups.getGroupManager()?.managedGroupIds()).toContain(
        created.group.id,
      );
      expect(bobGroups.getGroupManager()?.managedGroupIds()).toContain(
        created.group.id,
      );
      expect(sent.inner.content).toBe("hello from session runtime");
      expect(decrypted?.inner.content).toBe("hello from session runtime");
      expect(
        aliceGroups.getGroupManager()?.knownSenderEventPubkeys().length,
      ).toBeGreaterThan(0);
      expect(
        bobGroups.getGroupManager()?.knownSenderEventPubkeys().length,
      ).toBeGreaterThan(0);

      await bobGroups.syncGroups([]);
      expect(bobGroups.getGroupManager()?.managedGroupIds()).not.toContain(
        created.group.id,
      );
    } finally {
      aliceGroups.close();
      bobGroups.close();
    }
  });
});
