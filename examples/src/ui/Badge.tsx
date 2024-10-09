import socialGraph from "../utils/socialGraph"
import { CheckMarkIcon } from "./Icons"
import React from "react"

export const Badge = ({
  pubKey,
  className,
}: {
  pubKey: string
  className?: string
}) => {
  const distance = socialGraph().getFollowDistance(pubKey)

  if (distance <= 2) {
    let tooltip
    let badgeClass
    if (distance === 0) {
      tooltip = "You"
      badgeClass = "bg-purple-500"
    } else if (distance === 1) {
      tooltip = "Following"
      badgeClass = "bg-purple-500"
    } else if (distance === 2) {
      const followedByFriends = socialGraph().followedByFriends(pubKey)
      tooltip = `Followed by ${followedByFriends.size} friends`
      badgeClass = followedByFriends.size > 10 ? "bg-orange-500" : "bg-neutral"
    }
    return (
      <span
        className={`rounded-full aspect-square p-[2px] text-white ${badgeClass} ${className}`}
        title={tooltip}
      >
        <CheckMarkIcon size={12} />
      </span>
    )
  }
  return null
}
