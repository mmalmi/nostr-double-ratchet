import socialGraph from "../utils/socialGraph"
import {useEffect, useState} from "react"
import { NostrEvent } from "../../../src"
import {NDKEvent} from "@nostr-dev-kit/ndk"
import ndk from "../utils/ndk"

const useFollows = (pubKey: string, includeSelf = false) => {
  const [follows, setFollows] = useState([
    ...socialGraph().getFollowedByUser(pubKey, includeSelf),
  ])

  useEffect(() => {
    try {
      if (pubKey) {
        const filter = {kinds: [3], authors: [pubKey]}

        const sub = ndk.subscribe(filter)

        sub?.on("event", (event: NDKEvent) => {
          socialGraph().handleEvent(event as NostrEvent)
          setFollows([
            ...socialGraph().getFollowedByUser(pubKey, includeSelf),
          ])
        })
        return () => {
          sub.stop()
        }
      }
    } catch (error) {
      console.warn(error)
    }
  }, [pubKey, includeSelf])

  return follows
}

export default useFollows
