#!/usr/bin/env npx tsx
/**
 * E2E test helper: TypeScript "Iris-like" multi-device user <-> ndr CLI via WebSocket relay
 *
 * Usage: npx tsx e2e/ts-rust-multidevice-e2e.ts <relay_url>
 *
 * Flow:
 * - Device1 (owner device) publishes AppKeys with itself only
 * - Device1 creates a chat invite URL (ndr joins via URL)
 * - After Device1 receives "PING1", it:
 *   - adds Device2 to AppKeys (publishes update)
 *   - publishes Device2's Invite event (so peers can establish a session with Device2)
 * - Once Device2 session is established, the harness expects "PING2" to arrive on both devices.
 */

import { finalizeEvent, getPublicKey, generateSecretKey } from "nostr-tools";
import WebSocket from "ws";

// Use ws for Node.js WebSocket support (nostr-tools expects global WebSocket sometimes).
(global as any).WebSocket = WebSocket;

import { Invite } from "../src/Invite";
import { AppKeys } from "../src/AppKeys";
import { Session } from "../src/Session";

const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const RELAY_URL = process.argv[2];
if (!RELAY_URL) {
  log("Usage: npx tsx e2e/ts-rust-multidevice-e2e.ts <relay_url>");
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

async function main() {
  const relay = new SimpleRelay(RELAY_URL);
  await relay.waitReady();
  log(`E2E_RELAY_CONNECTED:${RELAY_URL}`);

  const subscribe = (filter: any, onEvent: (event: any) => void) => {
    relay.subscribe(filter, onEvent);
    return () => {};
  };

  // Device1 is the owner device (owner pubkey == device identity).
  const ownerSk = generateSecretKey();
  const ownerPubkey = getPublicKey(ownerSk);
  log(`E2E_OWNER_PUBKEY:${ownerPubkey}`);

  const publishAppKeys = async (devices: string[]) => {
    const keys = new AppKeys([]);
    for (const pk of devices) {
      keys.addDevice(keys.createDeviceEntry(pk));
    }
    const unsigned = keys.getEvent();
    unsigned.pubkey = ownerPubkey;
    const signed = finalizeEvent(unsigned, ownerSk);
    await relay.publish(signed);
    log(`E2E_APPKEYS_PUBLISHED:${devices.join(",")}`);
  };

  // Initial AppKeys: only owner device.
  await publishAppKeys([ownerPubkey]);

  // Create chat invite URL for ndr to join.
  const chatInvite = Invite.createNew(ownerPubkey);
  const inviteUrl = chatInvite.getUrl("https://example.com");
  log(`E2E_INVITE_URL:${inviteUrl}`);

  let session1: Session | null = null;
  let session2: Session | null = null;

  let device2Sk: Uint8Array | null = null;
  let device2Pubkey: string | null = null;
  let device2Invite: Invite | null = null;
  let device2SetupStarted = false;

  let ping2Device1 = false;
  let ping2Device2 = false;

  const maybeFinish = () => {
    if (ping2Device1 && ping2Device2) {
      log("E2E_SUCCESS");
      relay.close();
      process.exit(0);
    }
  };

  const attachSession1 = (s: Session, identity: string) => {
    if (session1) return;
    session1 = s;
    log(`E2E_SESSION1_CREATED:${identity}`);

    s.onEvent((msg) => {
      const content = msg.content;
      log(`E2E_DEVICE1_RECEIVED:${content}`);

      if (content === "PING1" && !device2SetupStarted) {
        device2SetupStarted = true;
        void (async () => {
          // Ensure AppKeys update has a later created_at than the initial publish.
          await sleep(1100);

          device2Sk = generateSecretKey();
          device2Pubkey = getPublicKey(device2Sk);
          log(`E2E_DEVICE2_PUBKEY:${device2Pubkey}`);

          // Update AppKeys to include Device2.
          await publishAppKeys([ownerPubkey, device2Pubkey]);

          // Device2 publishes its Invite event so peers can establish a session with it.
          device2Invite = Invite.createNew(device2Pubkey, device2Pubkey);
          const device2InviteEvent = finalizeEvent(device2Invite.getEvent(), device2Sk);
          await relay.publish(device2InviteEvent);
          log(`E2E_DEVICE2_INVITE_PUBLISHED:${device2InviteEvent.id}`);

          // Listen for invite responses to Device2's ephemeral key and create session2.
          device2Invite.listen(device2Sk, subscribe, (s2, identity2) => {
            if (session2) return;
            session2 = s2;
            log(`E2E_DEVICE2_SESSION_CREATED:${identity2}`);

            s2.onEvent((msg2) => {
              const c2 = msg2.content;
              log(`E2E_DEVICE2_RECEIVED:${c2}`);
              if (c2 === "PING2") {
                ping2Device2 = true;
                maybeFinish();
              }
            });
          });
        })();
      }

      if (content === "PING2") {
        ping2Device1 = true;
        maybeFinish();
      }
    });
  };

  // Listen for chat invite responses (creates session1).
  chatInvite.listen(ownerSk, subscribe, (s, identity) => {
    attachSession1(s, identity);
  });

  // Feed all double-ratchet message events to any established sessions.
  relay.subscribe({ kinds: [1060] }, (event: any) => {
    if (session1) {
      try {
        (session1 as any).handleNostrEvent(event);
      } catch {
        // ignore unrelated events
      }
    }
    if (session2) {
      try {
        (session2 as any).handleNostrEvent(event);
      } catch {
        // ignore unrelated events
      }
    }
  });

  // Wait for session establishment.
  const start = Date.now();
  const timeoutMs = 5 * 60_000;
  while (!session1 && Date.now() - start < timeoutMs) {
    await sleep(200);
  }

  if (!session1) {
    log("E2E_TIMEOUT_SESSION1");
    relay.close();
    process.exit(1);
  }

  // Now we just wait for the driving Rust test to send PING1 and PING2.
  while (Date.now() - start < timeoutMs) {
    await sleep(200);
  }

  log("E2E_TIMEOUT");
  relay.close();
  process.exit(1);
}

main().catch((e) => {
  log(`E2E_ERROR:${e?.message || e}`);
  process.exit(1);
});

