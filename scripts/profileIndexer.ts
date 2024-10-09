import NDK from "@nostr-dev-kit/ndk";
import fs from "fs";
import path from "path";
import throttle from "lodash/throttle";
import { SocialGraph, NostrEvent } from "../src";
import WebSocket from "ws";
import Fuse from "fuse.js";

global.WebSocket = WebSocket as any;

const DATA_DIR = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../data");
const SOCIAL_GRAPH_FILE = path.join(DATA_DIR, "socialGraph.json");
const FUSE_INDEX_FILE = path.join(DATA_DIR, "profileIndex.json");
const DATA_FILE = path.join(DATA_DIR, "profileData.json");

const SOCIAL_GRAPH_ROOT = "4523be58d395b1b196a9b8c82b038b6895cb02b683d0c253a955068dba1facd0";

let socialGraph: SocialGraph;
const fuse = new Fuse<Profile>([], { keys: ["name", "pubKey", "nip05"] });
const data: string[][] = [];

type Profile = {
  name: string;
  pubKey: string;
  nip05?: string;
};

const ndk = new NDK({
    explicitRelayUrls: ["wss://relay.snort.social", "wss://relay.damus.io", "wss://relay.nostr.band", "wss://nostr.wine", "wss://soloco.nl", "wss://eden.nostr.land"],
});

const MAX_NAME_LENGTH = 100

const savefuse = throttle(async () => {
    try {
      fs.writeFileSync(FUSE_INDEX_FILE, JSON.stringify(fuse.getIndex().toJSON()));
      fs.writeFileSync(DATA_FILE, JSON.stringify(data));
      console.log("Saved fuse index and data");
    } catch (e) {
      console.error("failed to serialize Fuse index or data", e);
    }
  }, 10000);

(async () => {
  await ndk.connect();
  console.log('ndk connected');

  let socialGraphData: string | null = null;

  if (fs.existsSync(SOCIAL_GRAPH_FILE)) {
    socialGraphData = fs.readFileSync(SOCIAL_GRAPH_FILE, "utf-8");
    console.log("Social graph file found and read");
  }

  if (socialGraphData) {
    try {
      socialGraph = new SocialGraph(SOCIAL_GRAPH_ROOT, JSON.parse(socialGraphData));
      console.log("Loaded social graph of size", socialGraph.size());
    } catch (e) {
      console.error("error deserializing", e);
      socialGraph = new SocialGraph(SOCIAL_GRAPH_ROOT);
    }
  } else {
    socialGraph = new SocialGraph(SOCIAL_GRAPH_ROOT);
    console.log("Initialized new social graph");
  }

  await fetchProfilesInBatches(socialGraph.userIterator(5));

  savefuse();
})();

async function fetchProfilesInBatches(iterator: Generator<string>) {
  let batch: string[] = [];
  for (const user of iterator) {
    batch.push(user);
    if (batch.length === 500) {
      console.log(`Processing batch of size ${batch.length}`);
      await processBatch(batch);
      batch = [];
    }
  }
  if (batch.length > 0) {
    console.log(`Processing final batch of size ${batch.length}`);
    await processBatch(batch);
    savefuse()
  }
}

async function processBatch(batch: string[]) {
  console.log(`Subscribing to profiles: ${batch.join(", ")}`);
  const sub = ndk.subscribe(
    {
      kinds: [0],
      authors: batch,
    },
    { closeOnEose: true }
  );
  sub.on("event", (e) => handleProfileEvent(e as NostrEvent));
  await new Promise((resolve) => setTimeout(resolve, 2000)); // Throttle requests
  console.log(`Processed batch of size ${batch.length}`);
}

const seen = new Set<string>() 
function handleProfileEvent(event: NostrEvent) {
  if (seen.has(event.pubkey)) {
    return
  }
  seen.add(event.pubkey)
  try {
    const profile = JSON.parse(event.content);
    const pubKey = event.pubkey;
    const name = (profile.display_name || profile.username).trim().slice(0, MAX_NAME_LENGTH);
    let nip05 = profile.nip05 ? (profile.nip05.split('@')[0].trim().toLowerCase().slice(0, MAX_NAME_LENGTH)) : undefined;
    if (nip05 === name.toLowerCase()) {
      nip05 = undefined
    }
  
    if (name) {
      console.log(`Handling profile event for ${name} (${pubKey})`);
      fuse.remove((profile) => profile.pubKey === pubKey);
      fuse.add({ name, pubKey, nip05 });
      const item = [pubKey, name]
      const hasPicture = profile.picture && profile.picture.length < 255
      if (nip05) {
        item.push(nip05)
      } else if (hasPicture) {
        item.push('')
      }
      if (hasPicture) {
        item.push(profile.picture.trim().replace(/^https:\/\//, ''));
      }
      data.push(item);
    }
  } catch (e) {
    console.error("Failed to handle profile event", e);
  }
}
