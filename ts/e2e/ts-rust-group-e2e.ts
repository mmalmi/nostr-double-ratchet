#!/usr/bin/env npx tsx
/**
 * E2E test: TypeScript group sender-keys <-> ndr CLI via WebSocket relay
 *
 * Usage: npx tsx e2e/ts-rust-group-e2e.ts <relay_url>
 *
 * Flow:
 * - TS creates an invite (Alice) and waits for ndr (Bob) to join.
 * - Bob sends a 1:1 message first (TS inviter is responder and cannot send first).
 * - TS sends group metadata over the 1:1 session to create the group on Bob.
 * - TS sends a sender-key one-to-many group message; ndr should decrypt it.
 * - ndr sends a sender-key one-to-many group message; TS should decrypt it.
 */

import WebSocket from "ws";
import { generateSecretKey, getPublicKey, type VerifiedEvent } from "nostr-tools";

// Use ws for Node.js WebSocket support.
(globalThis as any).WebSocket = WebSocket;

import { Invite } from "../src/Invite";
import { Group, GROUP_METADATA_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND, generateGroupSecret, type GroupData } from "../src/Group";
import { InMemoryStorageAdapter } from "../src/StorageAdapter";
import type { Rumor } from "../src/types";

// Force flush stdout for each line.
const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const RELAY_URL = process.argv[2];
if (!RELAY_URL) {
  log("Usage: npx tsx e2e/ts-rust-group-e2e.ts <relay_url>");
  process.exit(1);
}

class SimpleRelay {
  private ws: WebSocket;
  private subscriptions: Map<string, (event: any) => void> = new Map();
  private ready: Promise<void>;
  private subCounter = 0;

  constructor(url: string) {
    this.ws = new WebSocket(url);
    this.ready = new Promise((resolve, reject) => {
      this.ws.onopen = () => resolve();
      this.ws.onerror = (e: any) => reject(e);
    });

    this.ws.onmessage = (msg) => {
      try {
        const data = JSON.parse(msg.data.toString());
        if (data[0] === "EVENT" && data[1] && data[2]) {
          const subId = data[1];
          const event = data[2];
          const handler = this.subscriptions.get(subId);
          if (handler) handler(event);
        }
      } catch {
        // ignore parse errors
      }
    };
  }

  async waitReady() {
    await this.ready;
  }

  async publish(event: any) {
    await this.ready;
    this.ws.send(JSON.stringify(["EVENT", event]));
  }

  subscribe(filter: any, onEvent: (event: any) => void): string {
    const subId = `sub-${++this.subCounter}`;
    this.subscriptions.set(subId, onEvent);
    this.ws.send(JSON.stringify(["REQ", subId, filter]));
    return subId;
  }

  close() {
    this.ws.close();
  }
}

function hasTag(event: { tags?: string[][] }, key: string): boolean {
  return Boolean(event.tags?.some((t) => t[0] === key));
}

async function main() {
  // Alice (TypeScript side) identity key.
  const aliceSecretKey = generateSecretKey();
  const alicePubkey = getPublicKey(aliceSecretKey);
  log(`E2E_ALICE_PUBKEY:${alicePubkey}`);

  const relay = new SimpleRelay(RELAY_URL);
  await relay.waitReady();

  // Subscribe function for Invite/Session. (Relay is a toy, unsub not needed for test.)
  const subscribe = (filter: any, onEvent: (event: any) => void) => {
    relay.subscribe(filter, onEvent);
    return () => {};
  };

  // Create invite (Alice inviter).
  const invite = Invite.createNew(alicePubkey);
  const inviteUrl = invite.getUrl("https://example.com");
  log(`E2E_INVITE_URL:${inviteUrl}`);

  let bobPubkey: string | null = null;
  let groupId: string | null = null;
  let groupData: GroupData | null = null;
  let group: Group | null = null;
  let sentGroupBootstrap = false;
  let gotRustGroupMessage = false;

  const storage = new InMemoryStorageAdapter();

  // Listen for invite responses using Invite.listen().
  invite.listen(aliceSecretKey, subscribe, (session, identity) => {
    bobPubkey = identity;
    log(`E2E_SESSION_CREATED:${identity}`);

    // Prepare group state now; we will send it only after Bob sends first DM ("responder can't send first").
    groupId = crypto.randomUUID();
    groupData = {
      id: groupId,
      name: "E2E Group",
      members: [alicePubkey, identity],
      admins: [alicePubkey],
      createdAt: Date.now(),
      secret: generateGroupSecret(),
      accepted: true,
    };

    group = new Group({
      data: groupData,
      ourOwnerPubkey: alicePubkey,
      ourDevicePubkey: alicePubkey,
      storage,
    });

    // Handle decrypted 1:1 session rumors.
    //
    // NOTE: Session.onEvent() does not await async callbacks; wrap async work to avoid
    // unhandled promise rejections.
    session.onEvent((rumor) => {
      void (async () => {
        // Sender-key distribution from Bob (over 1:1 session) so we can decrypt Bob's one-to-many.
        if (
          group &&
          groupId &&
          bobPubkey &&
          rumor.kind === GROUP_SENDER_KEY_DISTRIBUTION_KIND &&
          rumor.tags?.some((t) => t[0] === "l" && t[1] === groupId)
        ) {
          const drained = await group.handleIncomingSessionEvent(rumor as Rumor, bobPubkey);
          for (const d of drained) {
            if (d.inner.content === "hello from rust group" && !gotRustGroupMessage) {
              gotRustGroupMessage = true;
              log(`E2E_GROUP_MESSAGE_RECEIVED:${d.inner.content}`);
              process.exit(0);
            }
          }
          return;
        }

        // First DM from Bob: now we can send group metadata + sender-keys bootstrap.
        if (!sentGroupBootstrap && group && groupId && groupData && bobPubkey) {
          sentGroupBootstrap = true;
          log(`E2E_HANDSHAKE_RECEIVED:${rumor.content}`);

          // 1) Send group metadata to Bob to create the group in ndr (routed via ["l", group_id]).
          const metadataContent = JSON.stringify({
            id: groupData.id,
            name: groupData.name,
            description: groupData.description,
            picture: groupData.picture,
            members: groupData.members,
            admins: groupData.admins,
            secret: groupData.secret,
          });

          const meta = session.sendEvent({
            kind: GROUP_METADATA_KIND,
            content: metadataContent,
            tags: [["l", groupId]],
            pubkey: alicePubkey,
          });
          await relay.publish(meta.event);
          log(`E2E_GROUP_METADATA_SENT:${groupId}`);

          // 2) Send a sender-key one-to-many group message.
          const sendPairwise = async (_to: string, r: Rumor) => {
            const dm = session.sendEvent({
              kind: r.kind,
              content: r.content,
              tags: r.tags,
              created_at: r.created_at,
              pubkey: r.pubkey,
            });
            await relay.publish(dm.event);
          };

          const publishOuter = async (outer: VerifiedEvent) => {
            await relay.publish(outer);
          };

          await group.sendMessage("hello from ts group", { sendPairwise, publishOuter });
          log(`E2E_TS_GROUP_MESSAGE_SENT:${groupId}`);
        }
      })().catch((e) => {
        log(`E2E_ERROR:${e?.message || e}`);
        process.exit(1);
      });
    });
  });

  // Subscribe to all kind=1060 events, but only route non-session messages (no "header" tag) to Group.
  // Session traffic is handled by the Session's own subscriptions.
  relay.subscribe({ kinds: [1060] }, (event: any) => {
    void (async () => {
      if (!group || !groupId) return;
      if (hasTag(event, "header")) return; // 1:1 session messages

      const dec = await group.handleOuterEvent(event as any);
      if (!dec) return;

      // Success condition: decrypt Bob's message.
      if (dec.inner.content === "hello from rust group" && !gotRustGroupMessage) {
        gotRustGroupMessage = true;
        log(`E2E_GROUP_MESSAGE_RECEIVED:${dec.inner.content}`);
        process.exit(0);
      }
    })().catch((e) => {
      log(`E2E_ERROR:${e?.message || e}`);
      process.exit(1);
    });
  });

  // Timeout.
  setTimeout(() => {
    if (!gotRustGroupMessage) {
      log("E2E_TIMEOUT");
      relay.close();
      process.exit(1);
    }
  }, 60_000).unref();
}

main().catch((e) => {
  log(`E2E_ERROR:${e?.message || e}`);
  process.exit(1);
});
