#!/usr/bin/env npx tsx
/**
 * E2E test for reactions and typing between TypeScript and ndr CLI
 *
 * Flow:
 * 1. TS creates invite, ndr joins
 * 2. ndr sends first message (ndr is initiator) -> TS receives it
 * 3. TS sends a reply -> ndr receives it
 * 4. ndr reacts to TS's reply, sends typing, then sends another message
 * 5. TS verifies it received the reaction, typing indicator, and follow-up
 */

import { getPublicKey, generateSecretKey } from "nostr-tools";
import WebSocket from "ws";

(global as any).WebSocket = WebSocket;

import { Invite } from "../src/Invite";
import { Session } from "../src/Session";
import { REACTION_KIND, TYPING_KIND, RECEIPT_KIND } from "../src/types";

const log = (msg: string) => {
  process.stdout.write(msg + "\n");
};

const RELAY_URL = process.argv[2];
if (!RELAY_URL) {
  log("Usage: npx tsx e2e/react-typing-e2e.ts <relay_url>");
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

  const aliceSecretKey = generateSecretKey();
  const alicePubkey = getPublicKey(aliceSecretKey);
  log(`E2E_ALICE_PUBKEY:${alicePubkey}`);

  const subscribe = (filter: any, onEvent: (event: any) => void) => {
    relay.subscribe(filter, onEvent);
    return () => {};
  };

  const invite = Invite.createNew(alicePubkey);
  const inviteUrl = invite.getUrl("https://example.com");
  log(`E2E_INVITE_URL:${inviteUrl}`);

  let session: Session | null = null;
  const receivedReactions: Array<{messageId: string, emoji: string}> = [];
  const receivedTyping: Array<{kind: number}> = [];
  const receivedMessages: Array<{id: string, content: string}> = [];

  invite.listen(aliceSecretKey, subscribe, (newSession, identity, deviceId) => {
    log(`E2E_SESSION_CREATED:identity=${identity}`);
    session = newSession;

    session.onEvent((rumor, outerEvent) => {
      const kind = rumor.kind;

      if (kind === REACTION_KIND) {
        const messageId = rumor.tags?.find((t: string[]) => t[0] === "e")?.[1] || "";
        const emoji = rumor.content;
        log(`E2E_REACTION_RECEIVED:messageId=${messageId},emoji=${emoji}`);
        receivedReactions.push({ messageId, emoji });
      } else if (kind === TYPING_KIND) {
        log(`E2E_TYPING_RECEIVED:kind=${kind}`);
        receivedTyping.push({ kind });
      } else if (kind === RECEIPT_KIND) {
        log(`E2E_RECEIPT_RECEIVED:type=${rumor.content}`);
      } else {
        const msgId = outerEvent?.id || rumor.id;
        log(`E2E_MESSAGE_RECEIVED:${rumor.content}`);
        receivedMessages.push({ id: msgId, content: rumor.content });
      }
    });
  });

  relay.subscribe({ kinds: [1060] }, (event: any) => {
    if (session) {
      try {
        (session as any).handleNostrEvent(event);
      } catch (e) {}
    }
  });

  log("E2E_LISTENING");

  const waitFor = async (pred: () => boolean, timeout: number, label: string): Promise<boolean> => {
    const start = Date.now();
    while (Date.now() - start < timeout) {
      if (pred()) return true;
      await new Promise(r => setTimeout(r, 100));
    }
    log(`E2E_TIMEOUT:${label}`);
    return false;
  };

  if (!await waitFor(() => session !== null, 30000, "session")) {
    process.exit(1);
  }

  // Wait for first message from ndr (ndr is initiator, must send first)
  if (!await waitFor(() => receivedMessages.length > 0, 30000, "first_message")) {
    process.exit(1);
  }
  log(`E2E_GOT_FIRST_MESSAGE:${receivedMessages[0].content}`);

  // TS sends a reply
  const { event: replyEvent, innerEvent: replyInner } = session!.send("Reply from TS!");
  log(`E2E_REPLY_SENT:id=${replyEvent.id}`);
  await relay.publish(replyEvent);

  // Wait for ndr to react to our reply, send typing, and send a follow-up
  if (!await waitFor(() => receivedReactions.length > 0, 30000, "reaction")) {
    process.exit(1);
  }
  log(`E2E_REACTION_OK:emoji=${receivedReactions[0].emoji},messageId=${receivedReactions[0].messageId}`);

  if (!await waitFor(() => receivedTyping.length > 0, 30000, "typing")) {
    process.exit(1);
  }
  log("E2E_TYPING_OK");

  // Wait for the follow-up message (message index 1, after the first one)
  if (!await waitFor(() => receivedMessages.length > 1, 30000, "followup")) {
    process.exit(1);
  }
  log(`E2E_FOLLOWUP_OK:${receivedMessages[1].content}`);

  log("E2E_ALL_OK");
  relay.close();
  process.exit(0);
}

main().catch((e) => {
  log(`E2E_ERROR:${e.message || e}`);
  process.exit(1);
});
