import classNames from "classnames"
import React, {useMemo} from "react"

import useProfile from "../hooks/useProfile.ts"

export function Name({pubKey, className}: {pubKey: string; className?: string}) {
  const profile = useProfile(pubKey)

  const name =
    profile?.name ||
    profile?.display_name ||
    profile?.username ||
    profile?.nip05?.split("@")[0]

  const animal = useMemo(() => {
    if (name) {
      return ""
    }
    if (!pubKey) {
      return ""
    }
    return pubKey.slice(0,8) + "..."
  }, [profile, pubKey])

  return (
    <span
      className={classNames(
        {
          italic: !!animal,
          "opacity-50": !!animal,
        },
        className
      )}
    >
      {name || animal}
    </span>
  )
}
