#!/usr/bin/env npx tsx
/**
 * E2E test: TypeScript <-> ndr CLI via WebSocket relay
 *
 * Usage: npx tsx e2e/ts-rust-e2e.ts <relay_url>
 *
 * This script:
 * 1. Connects to the provided relay
 * 2. Creates an invite using TypeScript
 * 3. Outputs the invite URL for ndr to join
 * 4. Waits for messages and outputs them
 */

import { getPublicKey, generateSecretKey } from "nostr-tools";
import WebSocket from "ws";

// Use ws for Node.js WebSocket support
(global as any).WebSocket = WebSocket;

import { Invite } from "../src/Invite";
import { Session } from "../src/Session";
import {
  createSessionFromAccept,
  decryptInviteResponse,
} from "../src/inviteUtils";
import { INVITE_RESPONSE_KIND } from "../src/types";

// Force flush stdout for each line
const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const RELAY_URL = process.argv[2];
if (!RELAY_URL) {
  log("Usage: npx tsx e2e/ts-rust-e2e.ts <relay_url>");
  process.exit(1);
}

// Generate keys for Alice (TypeScript side)
const aliceSecretKey = generateSecretKey();
const alicePubkey = getPublicKey(aliceSecretKey);

log(`E2E_ALICE_PUBKEY:${alicePubkey}`);
log(`E2E_CONNECTING:${RELAY_URL}`);

// Simple relay connection
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
        log(`E2E_WS_MSG:${data[0]}`);
        if (data[0] === "EVENT" && data[1] && data[2]) {
          const subId = data[1];
          const event = data[2];
          log(`E2E_WS_EVENT:sub=${subId},kind=${event.kind},id=${event.id?.slice(0,8)}`);
          const handler = this.subscriptions.get(subId);
          if (handler) {
            handler(event);
          } else {
            log(`E2E_NO_HANDLER_FOR:${subId}`);
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
    return () => {}; // unsubscribe not needed for test
  };

  // Create invite
  const invite = Invite.createNew(alicePubkey);
  const inviteUrl = invite.getUrl("https://example.com");
  log(`E2E_INVITE_URL:${inviteUrl}`);

  // Get the ephemeral key from the invite - responses are addressed to this key
  const ephemeralKey = invite.inviterEphemeralPublicKey;
  log(`E2E_EPHEMERAL_KEY:${ephemeralKey}`);

  // Listen for invite responses using the invite.listen() method
  let session: Session | null = null;
  const receivedMessages: string[] = [];
  const attachSession = (newSession: Session, identity: string) => {
    if (session) return;
    session = newSession;

    // Listen for messages on this session
    session.onEvent((msg) => {
      log(`E2E_MESSAGE_RECEIVED:${msg.content}`);
      receivedMessages.push(msg.content);

      // Send a reply back to ndr
      try {
        const reply = newSession.send("Hello from TypeScript!");
        const replyEventJson = JSON.stringify(reply.event);
        log(`E2E_REPLY_EVENT:${replyEventJson}`);
        // Publish the reply to the relay
        relay.publish(reply.event);
        log(`E2E_REPLY_SENT`);
      } catch (e: any) {
        log(`E2E_REPLY_ERROR:${e.message || e}`);
      }
    });

    // Signal readiness only after message callback is registered.
    log(`E2E_SESSION_CREATED:identity=${identity}`);
  };

  log(`E2E_LISTENING_FOR_RESPONSES`);
  invite.listen(
    aliceSecretKey,
    subscribe,
    (newSession, identity) => {
      attachSession(newSession, identity);
    }
  );

  // Fallback: listen to all invite responses and try to decrypt manually.
  // This avoids depending on relay-side tag filtering details in tests.
  relay.subscribe({ kinds: [INVITE_RESPONSE_KIND] }, (event: any) => {
    void (async () => {
      if (session) return;
      try {
        const decrypted = await decryptInviteResponse({
          envelopeContent: event.content,
          envelopeSenderPubkey: event.pubkey,
          inviterEphemeralPrivateKey: invite.inviterEphemeralPrivateKey!,
          inviterPrivateKey: aliceSecretKey,
          sharedSecret: invite.sharedSecret,
        });

        const fallbackSession = createSessionFromAccept({
          nostrSubscribe: subscribe,
          theirPublicKey: decrypted.inviteeSessionPublicKey,
          ourSessionPrivateKey: invite.inviterEphemeralPrivateKey!,
          sharedSecret: invite.sharedSecret,
          isSender: false,
          name: event.id,
        });
        attachSession(fallbackSession, decrypted.inviteeIdentity);
      } catch {
        // Ignore unrelated/undecryptable invite-response events.
      }
    })();
  });
  log("E2E_LISTENING_FOR_RESPONSES_READY");

  // Also listen for double-ratchet messages (kind 1060)
  relay.subscribe(
    { kinds: [1060] },
    (event: any) => {
      log(`E2E_DR_EVENT:${event.id.slice(0, 8)}`);
      if (session) {
        try {
          (session as any).handleNostrEvent(event);
        } catch (e) {
          // May not be for us
        }
      }
    }
  );

  log("E2E_LISTENING");

  // Wait for session establishment first, then wait for message delivery.
  const sessionTimeoutMs = 600_000;
  const messageTimeoutMs = 90_000;

  const sessionStart = Date.now();
  while (!session && Date.now() - sessionStart < sessionTimeoutMs) {
    await sleep(200);
  }

  if (!session) {
    log("E2E_TIMEOUT_SESSION");
    relay.close();
    process.exit(1);
  }

  const messageStart = Date.now();
  while (Date.now() - messageStart < messageTimeoutMs) {
    await sleep(200);
    if (receivedMessages.length > 0) {
      log(`E2E_SUCCESS:${receivedMessages.join(",")}`);
      relay.close();
      process.exit(0);
    }
  }

  log("E2E_TIMEOUT");
  relay.close();
  process.exit(1);
}

main().catch((e) => {
  log(`E2E_ERROR:${e.message || e}`);
  process.exit(1);
});
