#!/usr/bin/env npx tsx
/**
 * E2E helper:
 * - ndr creates invite URL
 * - iris-client (owner) publishes AppKeys with iris-chat linked device
 * - iris-client accepts invite URL
 * - iris-chat sends first message
 * - ndr replies and both iris-client + iris-chat receive it
 * - iris-client sends follow-up
 * - ndr replies again and both devices receive it
 *
 * Usage: npx tsx e2e/rust-ts-multidevice-e2e.ts <relay_url> <invite_url>
 */

import { finalizeEvent, getPublicKey, generateSecretKey } from "nostr-tools";
import WebSocket from "ws";

(global as any).WebSocket = WebSocket;

import { AppKeys } from "../src/AppKeys";
import { Invite } from "../src/Invite";
import { Session } from "../src/Session";

const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const RELAY_URL = process.argv[2];
const INVITE_URL = process.argv[3];
if (!RELAY_URL || !INVITE_URL) {
  log("Usage: npx tsx e2e/rust-ts-multidevice-e2e.ts <relay_url> <invite_url>");
  process.exit(1);
}

const IRIS_CHAT_TO_NDR_1 = "IRIS_CHAT_TO_NDR_1";
const IRIS_CLIENT_BOOTSTRAP = "IRIS_CLIENT_BOOTSTRAP";
const IRIS_CLIENT_ACK_KICKOFF = "IRIS_CLIENT_ACK_KICKOFF";
const IRIS_CLIENT_TO_NDR_2 = "IRIS_CLIENT_TO_NDR_2";
const NDR_KICKOFF = "NDR_KICKOFF";
const NDR_TO_IRIS_1 = "NDR_TO_IRIS_1";
const NDR_TO_IRIS_2 = "NDR_TO_IRIS_2";

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
          if (handler) {
            handler(event);
          }
        }
      } catch {
        // Ignore parse errors from relay.
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

  const ownerSk = generateSecretKey();
  const ownerPubkey = getPublicKey(ownerSk);
  log(`E2E_OWNER_PUBKEY:${ownerPubkey}`);

  const device2Sk = generateSecretKey();
  const device2Pubkey = getPublicKey(device2Sk);
  log(`E2E_DEVICE2_PUBKEY:${device2Pubkey}`);

  const subscribe = (filter: any, onEvent: (event: any) => void) => {
    relay.subscribe(filter, onEvent);
    return () => {};
  };

  let ownerSession: Session | null = null;
  let device2Session: Session | null = null;

  const ownerReceived = new Set<string>();
  const device2Received = new Set<string>();

  const onOwnerEvent = (msg: { content: string }) => {
    ownerReceived.add(msg.content);
    log(`E2E_OWNER_RECEIVED:${msg.content}`);
  };

  const onDevice2Event = (msg: { content: string }) => {
    device2Received.add(msg.content);
    log(`E2E_DEVICE2_RECEIVED:${msg.content}`);
  };

  const publishAppKeys = async () => {
    const keys = new AppKeys([]);
    keys.addDevice(keys.createDeviceEntry(ownerPubkey));
    keys.addDevice(keys.createDeviceEntry(device2Pubkey));

    const unsigned = keys.getEvent();
    unsigned.pubkey = ownerPubkey;
    const signed = finalizeEvent(unsigned, ownerSk);
    await relay.publish(signed);
    log(`E2E_APPKEYS_PUBLISHED:${ownerPubkey},${device2Pubkey}`);
  };

  const device2Invite = Invite.createNew(device2Pubkey, device2Pubkey);
  device2Invite.listen(device2Sk, subscribe, (session, identity) => {
    if (device2Session) return;
    device2Session = session;
    device2Session.onEvent(onDevice2Event);
    log(`E2E_DEVICE2_SESSION_CREATED:${identity}`);
  });

  await publishAppKeys();

  const device2InviteEvent = finalizeEvent(device2Invite.getEvent(), device2Sk);
  await relay.publish(device2InviteEvent);
  log(`E2E_DEVICE2_INVITE_PUBLISHED:${device2InviteEvent.id}`);

  const invite = Invite.fromUrl(INVITE_URL);
  const accepted = await invite.accept(subscribe, ownerPubkey, ownerSk);
  ownerSession = accepted.session;
  ownerSession.onEvent(onOwnerEvent);
  log("E2E_OWNER_SESSION_CREATED");

  await relay.publish(accepted.event);
  log("E2E_INVITE_RESPONSE_PUBLISHED");

  relay.subscribe({ kinds: [1060] }, (event: any) => {
    if (ownerSession) {
      try {
        (ownerSession as any).handleNostrEvent(event);
      } catch {
        // ignore unrelated events
      }
    }
    if (device2Session) {
      try {
        (device2Session as any).handleNostrEvent(event);
      } catch {
        // ignore unrelated events
      }
    }
  });

  let sentByDevice2 = false;
  let sentOwnerBootstrap = false;
  let sentOwnerAck = false;
  let sentByOwner = false;
  const start = Date.now();
  const timeoutMs = 180_000;

  while (Date.now() - start < timeoutMs) {
    if (ownerSession && device2Session && !sentOwnerBootstrap) {
      const sent = ownerSession.send(IRIS_CLIENT_BOOTSTRAP);
      await relay.publish(sent.event);
      sentOwnerBootstrap = true;
      log(`E2E_OWNER_BOOTSTRAP_SENT:${IRIS_CLIENT_BOOTSTRAP}`);
    }

    if (
      ownerSession &&
      device2Session &&
      sentOwnerBootstrap &&
      !sentOwnerAck &&
      ownerReceived.has(NDR_KICKOFF)
    ) {
      const sent = ownerSession.send(IRIS_CLIENT_ACK_KICKOFF);
      await relay.publish(sent.event);
      sentOwnerAck = true;
      log(`E2E_OWNER_ACK_SENT:${IRIS_CLIENT_ACK_KICKOFF}`);
    }

    if (
      ownerSession &&
      device2Session &&
      !sentByDevice2 &&
      sentOwnerBootstrap &&
      sentOwnerAck &&
      device2Received.has(NDR_KICKOFF)
    ) {
      const sent = device2Session.send(IRIS_CHAT_TO_NDR_1);
      await relay.publish(sent.event);
      sentByDevice2 = true;
      log(`E2E_DEVICE2_SENT:${IRIS_CHAT_TO_NDR_1}`);
    }

    if (
      ownerSession &&
      !sentByOwner &&
      ownerReceived.has(NDR_TO_IRIS_1) &&
      device2Received.has(NDR_TO_IRIS_1)
    ) {
      const sent = ownerSession.send(IRIS_CLIENT_TO_NDR_2);
      await relay.publish(sent.event);
      sentByOwner = true;
      log(`E2E_OWNER_SENT:${IRIS_CLIENT_TO_NDR_2}`);
    }

    if (ownerReceived.has(NDR_TO_IRIS_2) && device2Received.has(NDR_TO_IRIS_2)) {
      log("E2E_SUCCESS");
      relay.close();
      process.exit(0);
    }

    await sleep(100);
  }

  log("E2E_TIMEOUT");
  relay.close();
  process.exit(1);
}

main().catch((e) => {
  log(`E2E_ERROR:${e?.message || e}`);
  process.exit(1);
});
