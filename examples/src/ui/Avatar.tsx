import React, {useEffect, useMemo, useState} from "react"

import MinidenticonImg from "./MinidenticonImg.tsx"
import ProxyImg from "./ProxyImg.tsx"
import useProfile from "../hooks/useProfile.ts"
import {Badge} from "./Badge"

export const Avatar = ({
  width = 45,
  pubKey,
  showBadge = true,
  showTooltip = true,
}: {
  width?: number
  pubKey: string
  showBadge?: boolean
  showTooltip?: boolean
  showHoverCard?: boolean
}) => {
  const profile = useProfile(pubKey)
  const [image, setImage] = useState(String(profile?.picture || ""))

  useEffect(() => {
    const fetchImage = async () => {
      if (profile?.picture) {
        setImage(String(profile.picture))
      } else {
        setImage("")
      }
    }

    fetchImage()
  }, [profile])

  const handleImageError = () => {
    setImage("")
  }

  return (
    <div
      className={`aspect-square rounded-full bg-base-100 flex items-center justify-center select-none relative`}
      style={{width, height: width}}
    >
      {showBadge && (
        <Badge
          pubKey={pubKey}
          className="absolute top-0 right-0 transform translate-x-1/3 -translate-y-1/3"
        />
      )}
      <div
        className="w-full rounded-full overflow-hidden aspect-square not-prose"
        title={
          showTooltip
            ? String(
                profile?.name ||
                  profile?.display_name ||
                  profile?.username ||
                  profile?.nip05?.split("@")[0] ||
                  pubKey.slice(0,8) + "..."
              )
            : ""
        }
      >
        {image ? (
          <ProxyImg
            width={width}
            square={true}
            src={image}
            alt="User Avatar"
            className="w-full h-full object-cover"
            onError={handleImageError}
          />
        ) : (
          <MinidenticonImg username={pubKey} />
        )}
      </div>
    </div>
  )
}
