import React, { useState, useEffect, useCallback } from "react";
import { Avatar } from "../ui/Avatar";
import socialGraph, {saveGraph} from "../utils/socialGraph";
import ndk from "../utils/ndk";
import { throttle } from "lodash";
import { Link } from "react-router-dom";
import { nip19 } from "nostr-tools";

interface ExploreProps {
  pubKey: string;
  selectedUser: string;
}

interface FollowDistance {
  distance: number;
  count: number;
  randomUsers: string[];
}

const RANDOM_USER_COUNT = 3

const Explore = ({ pubKey, selectedUser }: ExploreProps) => {
  const [followDistances, setFollowDistances] = useState<FollowDistance[]>([]);

  const generateFollowDistances = (prevDistances: FollowDistance[] = []) => {
    return [0,1,2,3,4,5].map(d => {
      const users = Array.from(socialGraph().getUsersByFollowDistance(d));
      let randomUsers;
      const prevDistance = prevDistances.find(pd => pd.distance === d);
      if (prevDistance && prevDistance.randomUsers.length >= RANDOM_USER_COUNT) {
        randomUsers = prevDistance.randomUsers;
      } else {
        randomUsers = users.sort(() => 0.5 - Math.random()).slice(0, RANDOM_USER_COUNT);
      }
      return {
        distance: d,
        count: users.length,
        randomUsers
      };
    }).filter(d => d.count > 0);
  };

  const debouncedSetFollowDistances = useCallback(throttle(() => {
    setFollowDistances(prevDistances => generateFollowDistances(prevDistances));
  }, 1000), [])

  useEffect(() => {
    socialGraph().setRoot(pubKey);
    setFollowDistances(generateFollowDistances());

    const missing = [] as string[];
    for (const k of socialGraph().getUsersByFollowDistance(1).values()) {
      if (socialGraph().getFollowedByUser(k).size === 0) {
        missing.push(k);
      }
    }
    const sub = ndk.subscribe({ kinds: [3], authors: missing });
    sub.on("event", (e) => {
      socialGraph().handleEvent(e);
      saveGraph()
      debouncedSetFollowDistances();
    });
    return () => sub.stop();
  }, [pubKey, selectedUser]);

  return (
    <div className="flex flex-col gap-4 p-4 rounded-xl bg-base-100">
        <div className="flex flex-row flex-wrap justify-between items-start gap-4 uppercase text-xs text-base-content/50 font-bold">
            Your graph by follow distance
        </div>
      {followDistances.length === 0 && <div className="flex p-4 items-center justify-center">None</div>}
      {followDistances.map((d) => (
        <div key={d.distance} className="flex flex-row justify-between items-center my-4">
          <div className="text-xl flex flex-col items-center justify-center w-12">
            <b>{d.distance}</b>
            <div className="text-base-content/50 text-base">{d.count}</div>
          </div>
          <div className="flex flex-row gap-2 items-center text-lg">
            {d.randomUsers.map((user) => (
              <Link key={user} className="cursor-pointer" to={`/${nip19.npubEncode(user)}`}>
                  <Avatar pubKey={user} />
              </Link>
            ))}
            {d.distance > 0 && d.count > d.randomUsers.length && "+"}
          </div>
        </div>
      ))}
    </div>
  );
};

export default Explore;