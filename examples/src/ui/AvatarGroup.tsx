import React from "react"
import {Avatar} from "./Avatar.tsx"

export function AvatarGroup({
  pubKeys,
  onClick,
  avatarWidth = 30,
}: {
  pubKeys: string[]
  avatarWidth?: number
  onClick?: () => void
}) {
  return (
    <div className="flex overflow-hidden">
      {pubKeys.map((a, index) => (
        <div
          onClick={onClick}
          className={`flex-shrink-0 ${index > 0 ? "-ml-2" : ""}`}
          key={a}
          style={{zIndex: pubKeys.length - index}}
        >
          <Avatar showBadge={false} pubKey={a} width={avatarWidth} />
        </div>
      ))}
    </div>
  )
}
