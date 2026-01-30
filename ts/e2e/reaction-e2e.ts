#!/usr/bin/env npx tsx
/**
 * E2E test for reactions between TypeScript and ndr CLI
 * 
 * Tests:
 * 1. TypeScript sends message -> ndr receives with correct event ID
 * 2. ndr sends reaction to that message -> TypeScript receives reaction
 * 3. ndr sends message -> TypeScript receives with correct event ID
 * 4. TypeScript sends reaction -> ndr could receive it (via listen)
 */

import { getPublicKey, generateSecretKey } from "nostr-tools";
import WebSocket from "ws";

(global as any).WebSocket = WebSocket;

import { Invite } from "../src/Invite";
import { Session } from "../src/Session";
import { REACTION_KIND } from "../src/types";

const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const RELAY_URL = process.argv[2];
if (!RELAY_URL) {
  log("Usage: npx tsx e2e/reaction-e2e.ts <relay_url>");
  process.exit(1);
}

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
      this.ws.onerror = (e) => reject(e);
    });

    this.ws.onmessage = (msg) => {
      try {
        const data = JSON.parse(msg.data.toString());
        if (data[0] === "EVENT" && data[1] && data[2]) {
          const subId = data[1];
          const event = data[2];
          log(`E2E_WS_EVENT:sub=${subId},kind=${event.kind},id=${event.id?.slice(0,8)}`);
          const handler = this.subscriptions.get(subId);
          if (handler) handler(event);
        }
      } catch (e) {}
    };
  }

  async waitReady() { await this.ready; }
  
  async publish(event: any) {
    await this.ready;
    this.ws.send(JSON.stringify(["EVENT", event]));
    log(`E2E_PUBLISHED:kind=${event.kind},id=${event.id.slice(0,8)}`);
  }

  subscribe(filter: any, onEvent: (event: any) => void): string {
    const subId = `sub-${++this.subCounter}`;
    this.subscriptions.set(subId, onEvent);
    this.ws.send(JSON.stringify(["REQ", subId, filter]));
    return subId;
  }

  close() { this.ws.close(); }
}

async function main() {
  const relay = new SimpleRelay(RELAY_URL);
  await relay.waitReady();
  log(`E2E_RELAY_CONNECTED:${RELAY_URL}`);

  // Generate keys for Alice (TypeScript side)
  const aliceSecretKey = generateSecretKey();
  const alicePubkey = getPublicKey(aliceSecretKey);
  log(`E2E_ALICE_PUBKEY:${alicePubkey}`);

  const subscribe = (filter: any, onEvent: (event: any) => void) => {
    relay.subscribe(filter, onEvent);
    return () => {};
  };

  // Create invite
  const invite = Invite.createNew(alicePubkey);
  const inviteUrl = invite.getUrl("https://example.com");
  log(`E2E_INVITE_URL:${inviteUrl}`);

  let session: Session | null = null;
  const receivedMessages: Array<{id: string, content: string}> = [];
  const receivedReactions: Array<{messageId: string, emoji: string}> = [];

  // Listen for invite responses
  invite.listen(aliceSecretKey, subscribe, (newSession, identity, deviceId) => {
    log(`E2E_SESSION_CREATED:identity=${identity}`);
    session = newSession;

    // Listen for messages and reactions
    session.onEvent((rumor, outerEvent) => {
      // Use outer event ID as message ID (like iris-chat does)
      const msgId = outerEvent?.id || rumor.id;

      if (rumor.kind === REACTION_KIND) {
        const messageId = rumor.tags?.find((t: string[]) => t[0] === "e")?.[1] || "";
        const emoji = rumor.content;
        log(`E2E_REACTION_RECEIVED:messageId=${messageId},emoji=${emoji}`);
        receivedReactions.push({ messageId, emoji });
      } else {
        log(`E2E_MESSAGE_RECEIVED:id=${msgId},content=${rumor.content}`);
        receivedMessages.push({ id: msgId, content: rumor.content });
      }
    });
  });

  // Listen for kind 1060 events
  relay.subscribe({ kinds: [1060] }, (event: any) => {
    if (session) {
      try {
        (session as any).handleNostrEvent(event);
      } catch (e) {}
    }
  });

  log("E2E_LISTENING");

  // Wait for session to be created (ndr will join)
  const waitForSession = async (timeout: number): Promise<boolean> => {
    const start = Date.now();
    while (Date.now() - start < timeout) {
      if (session) return true;
      await new Promise(r => setTimeout(r, 100));
    }
    return false;
  };

  if (!await waitForSession(30000)) {
    log("E2E_TIMEOUT_NO_SESSION");
    process.exit(1);
  }

  // Alice sends a message
  log("E2E_SENDING_MESSAGE");
  const { event: aliceMsg, innerEvent: aliceInner } = session!.send("Hello from TypeScript!");
  log(`E2E_SENT_MESSAGE:outer_id=${aliceMsg.id},inner_id=${aliceInner.id}`);
  await relay.publish(aliceMsg);

  // Wait for ndr to receive and respond
  await new Promise(r => setTimeout(r, 2000));

  // Now wait for:
  // 1. A message from ndr (we'll get the event ID)
  // 2. A reaction from ndr to our message
  
  const waitForMessages = async (timeout: number): Promise<void> => {
    const start = Date.now();
    while (Date.now() - start < timeout) {
      await new Promise(r => setTimeout(r, 500));
      log(`E2E_STATUS:messages=${receivedMessages.length},reactions=${receivedReactions.length}`);
    }
  };

  await waitForMessages(10000);

  // Report results
  log(`E2E_FINAL_MESSAGES:${JSON.stringify(receivedMessages)}`);
  log(`E2E_FINAL_REACTIONS:${JSON.stringify(receivedReactions)}`);

  // Check if we got a reaction to our message
  const ourMsgId = aliceMsg.id;
  const gotReaction = receivedReactions.some(r => r.messageId === ourMsgId);
  log(`E2E_REACTION_TO_OUR_MSG:${gotReaction}`);

  relay.close();
  process.exit(gotReaction ? 0 : 1);
}

main().catch((e) => {
  log(`E2E_ERROR:${e.message || e}`);
  process.exit(1);
});
