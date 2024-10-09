import React, {Fragment, useMemo} from "react"

import {AvatarGroup} from "./AvatarGroup"
import {Name} from "./Name"

import socialGraph from "../utils/socialGraph"
import {Badge} from "./Badge"

const MAX_FOLLOWED_BY_FRIENDS = 3

// Extracted function
const getFollowedByFriends = (pubkey: string, max: number) => {
  const followedByFriends = socialGraph().followedByFriends(pubkey)
  return {
    followedByFriendsArray: Array.from(followedByFriends).slice(
      0,
      max
    ),
    totalFollowedByFriends: followedByFriends.size,
  }
}

export default function FollowedBy({pubkey}: {pubkey: string}) {
  const followDistance = socialGraph().getFollowDistance(pubkey)
  const {followedByFriendsArray, totalFollowedByFriends} = useMemo(() => getFollowedByFriends(pubkey, MAX_FOLLOWED_BY_FRIENDS), [pubkey, followDistance])

  const renderFollowedByFriendsLinks = () => {
    return followedByFriendsArray.map((a, index) => (
      <Fragment key={a}>
        <Name pubKey={a} />
        {index < followedByFriendsArray.length - 1 && ","}{" "}
      </Fragment>
    ))
  }

  return (
    <div className="flex flex-row items-center gap-2 text-sm text-base-content/50">
      <Badge pubKey={pubkey} />
      {!!followedByFriendsArray.length && (
        <div className="flex flex-row items-center">
          <AvatarGroup pubKeys={followedByFriendsArray} avatarWidth={20} />
        </div>
      )}
      {totalFollowedByFriends > 0 && (
        <div className="text-base-content/50">
          <span className="mr-1">Followed by</span>
          {renderFollowedByFriendsLinks()}
          {totalFollowedByFriends > MAX_FOLLOWED_BY_FRIENDS && (
            <span>
              and {totalFollowedByFriends - MAX_FOLLOWED_BY_FRIENDS} others you follow
            </span>
          )}
        </div>
      )}
      {followDistance !== 3 && totalFollowedByFriends < 1 && (
        <div className="text-gray-light">Not followed by anyone you follow</div>
      )}
      {followDistance === 3 && (
        <div className="text-gray-light">Followed by friends of friends</div>
      )}
    </div>
  )
}
