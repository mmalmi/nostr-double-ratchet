import React, {useMemo} from "react"
import {minidenticon} from "minidenticons"

type Props = {
  username: string
  saturation?: number
  lightness?: number
}

const MinidenticonImg = ({username, saturation, lightness}: Props) => {
  const svgURI = useMemo(
    () =>
      "data:image/svg+xml;utf8," +
      encodeURIComponent(minidenticon(username, saturation, lightness)),
    [username, saturation, lightness]
  )
  return <img src={svgURI} alt={username} />
}

export default MinidenticonImg
