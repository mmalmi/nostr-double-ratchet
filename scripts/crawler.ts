import NDK from "@nostr-dev-kit/ndk";
import fs from "fs";
import path from "path";
import throttle from "lodash/throttle";
import { SocialGraph, NostrEvent } from "../src";
import WebSocket from "ws";

global.WebSocket = WebSocket as any;

const MAX_SOCIAL_GRAPH_SERIALIZE_SIZE = 10 * 1024 * 1024;
const DATA_DIR = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../data");
const SOCIAL_GRAPH_FILE = path.join(DATA_DIR, "socialGraph.json");

const SOCIAL_GRAPH_ROOT = "4523be58d395b1b196a9b8c82b038b6895cb02b683d0c253a955068dba1facd0";

let socialGraph: SocialGraph;

const throttledSave = throttle(async () => {
  try {
    if (!fs.existsSync(DATA_DIR)) {
      fs.mkdirSync(DATA_DIR);
    }
    fs.writeFileSync(SOCIAL_GRAPH_FILE, JSON.stringify(socialGraph.serialize(MAX_SOCIAL_GRAPH_SERIALIZE_SIZE)));
    console.log("Saved social graph of size", socialGraph.size());
  } catch (e) {
    console.error("failed to serialize SocialGraph", e);
    console.log("social graph size", socialGraph.size());
  }
}, 10000);

const ndk = new NDK({
    explicitRelayUrls: ["wss://relay.snort.social", "wss://relay.damus.io", "wss://relay.nostr.band", "wss://nostr.wine", "wss://soloco.nl", "wss://eden.nostr.land"],
});

(async () => {
  await ndk.connect();
  console.log('ndk connected')

  let socialGraphData: string | null = null;

  if (fs.existsSync(SOCIAL_GRAPH_FILE)) {
    socialGraphData = fs.readFileSync(SOCIAL_GRAPH_FILE, "utf-8");
  }

  if (socialGraphData) {
    try {
      socialGraph = new SocialGraph(SOCIAL_GRAPH_ROOT, JSON.parse(socialGraphData));
      console.log("Loaded social graph of size", socialGraph.size());
      console.log("socialGraph size", socialGraphData.length / 1000, "KB");
    } catch (e) {
      console.error("error deserializing", e);
      socialGraph = new SocialGraph(SOCIAL_GRAPH_ROOT);
    }
  } else {
    socialGraph = new SocialGraph(SOCIAL_GRAPH_ROOT);
  }

  const event = await ndk.fetchEvent({
    kinds: [3],
    authors: [SOCIAL_GRAPH_ROOT],
    limit: 1,
  });

  if (event) {
    handleFollowEvent(event as NostrEvent);
    getMissingFollowLists(SOCIAL_GRAPH_ROOT);
  } else {
    console.log('No root follow event found');
  }
})();

function getMissingFollowLists(myPubKey: string) {
  const myFollows = socialGraph.getFollowedByUser(myPubKey);
  const missing = new Set<string>();
  for (const k of myFollows) {
    if (socialGraph.getFollowedByUser(k).size === 0) {
      missing.add(k);
    }
  }
  console.log("fetching", missing.size, "missing follow lists");

  const fetchBatch = (authors: string[]) => {
    const sub = ndk.subscribe(
      {
        kinds: [3],
        authors: authors,
      },
      { closeOnEose: true }
    );
    sub.on("event", (e) => handleFollowEvent(e as NostrEvent));
  };

  const processMissing = () => {
    const batch = [...missing].slice(0, 500);
    if (batch.length > 0) {
      fetchBatch(batch);
      batch.forEach((author) => missing.delete(author));
      if (missing.size > 0) {
        setTimeout(processMissing, 5000);
      }
    }
  };

  processMissing();
}

function handleFollowEvent(event: NostrEvent) {
  socialGraph.handleEvent(event);
  throttledSave();
}
