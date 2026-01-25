#!/usr/bin/env npx tsx
/**
 * E2E test: ndr CLI creates invite <-> TypeScript accepts
 *
 * This is the reverse direction of ts-rust-e2e.ts
 * Tests that ndr listen properly receives invite responses.
 *
 * Usage: npx tsx e2e/rust-ts-e2e.ts <relay_url> <invite_url>
 */

import { getPublicKey, generateSecretKey } from "nostr-tools";
import WebSocket from "ws";

// Use ws for Node.js WebSocket support
(global as any).WebSocket = WebSocket;

import { Invite } from "../src/Invite";
import { Session } from "../src/Session";

const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const RELAY_URL = process.argv[2];
const INVITE_URL = process.argv[3];

if (!RELAY_URL || !INVITE_URL) {
  log("Usage: npx tsx e2e/rust-ts-e2e.ts <relay_url> <invite_url>");
  process.exit(1);
}

// Generate keys for Bob (TypeScript side - the one accepting the invite)
const bobSecretKey = generateSecretKey();
const bobPubkey = getPublicKey(bobSecretKey);

log(`E2E_BOB_PUBKEY:${bobPubkey}`);
log(`E2E_CONNECTING:${RELAY_URL}`);

class SimpleRelay {
  private ws: WebSocket;
  private subscriptions: Map<string, (event: any) => void> = new Map();
  private ready: Promise<void>;
  private subCounter = 0;

  constructor(url: string) {
    this.ws = new WebSocket(url);
    this.ready = new Promise((resolve, reject) => {
      this.ws.onopen = () => {
        log("E2E_WS_OPEN");
        resolve();
      };
      this.ws.onerror = (e) => {
        log(`E2E_WS_ERROR:${e.message || "unknown"}`);
        reject(e);
      };
    });

    this.ws.onmessage = (msg) => {
      try {
        const data = JSON.parse(msg.data.toString());
        if (data[0] === "EVENT" && data[1] && data[2]) {
          const subId = data[1];
          const event = data[2];
          log(`E2E_WS_EVENT:sub=${subId},kind=${event.kind},id=${event.id?.slice(0,8)}`);
          const handler = this.subscriptions.get(subId);
          if (handler) {
            handler(event);
          }
        }
      } catch (e) {
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
    log(`E2E_PUBLISHED:kind=${event.kind}`);
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

  // Create subscribe function for Session
  const subscribe = (filter: any, onEvent: (event: any) => void) => {
    relay.subscribe(filter, onEvent);
    return () => {};
  };

  // Parse the invite URL from ndr
  log(`E2E_PARSING_INVITE:${INVITE_URL}`);
  const invite = Invite.fromUrl(INVITE_URL);
  log(`E2E_INVITE_PARSED:inviter=${invite.inviter}`);
  log(`E2E_INVITE_EPHEMERAL:${invite.inviterEphemeralPublicKey}`);

  // Accept the invite - this creates a session and a response event
  const { session, event: responseEvent } = await invite.accept(
    subscribe,
    bobPubkey,
    bobSecretKey
  );
  log(`E2E_INVITE_ACCEPTED`);
  log(`E2E_RESPONSE_EVENT:${JSON.stringify(responseEvent)}`);

  // The response event is tagged with 'p' = ephemeral key
  const pTag = responseEvent.tags.find((t: any) => t[0] === 'p');
  log(`E2E_RESPONSE_P_TAG:${pTag ? pTag[1] : 'none'}`);

  // Publish the response event to the relay
  // This is what ndr listen should pick up!
  await relay.publish(responseEvent);
  log(`E2E_RESPONSE_PUBLISHED`);

  // Listen for messages from ndr
  const receivedMessages: string[] = [];
  session.onEvent((msg) => {
    log(`E2E_MESSAGE_RECEIVED:${msg.content}`);
    receivedMessages.push(msg.content);
  });

  // Also subscribe to message events
  relay.subscribe(
    { kinds: [1060] },
    (event: any) => {
      log(`E2E_DR_EVENT:${event.id.slice(0, 8)}`);
      try {
        (session as any).handleNostrEvent(event);
      } catch (e) {
        // May not be for us
      }
    }
  );

  log("E2E_LISTENING");

  // Wait a bit to see if we get any messages
  const timeout = 30000;
  const startTime = Date.now();

  while (Date.now() - startTime < timeout) {
    await new Promise(r => setTimeout(r, 1000));
    if (receivedMessages.length > 0) {
      log(`E2E_SUCCESS:${receivedMessages.join(",")}`);
      break;
    }
  }

  if (receivedMessages.length === 0) {
    log("E2E_TIMEOUT_NO_MESSAGES");
  }

  relay.close();
  // Exit successfully if we were able to accept the invite and publish response
  // The main test is that ndr listen can receive the response event
  process.exit(0);
}

main().catch((e) => {
  log(`E2E_ERROR:${e.message || e}`);
  process.exit(1);
});
