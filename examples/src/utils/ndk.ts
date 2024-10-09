import NDK from "@nostr-dev-kit/ndk"

const ndk = new NDK({
    explicitRelayUrls: [
      "wss://relay.snort.social",
      "wss://relay.damus.io",
      "wss://relay.nostr.band",
      "wss://nostr.wine",
      "wss://soloco.nl",
      "wss://eden.nostr.land",
    ],
  })
  ndk.connect()

  export default ndk