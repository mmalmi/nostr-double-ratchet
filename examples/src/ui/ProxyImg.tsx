import {CSSProperties, useEffect, useState, MouseEvent} from "react"

type Props = {
  src: string
  className?: string
  style?: CSSProperties
  width?: number
  square?: boolean
  onError?: () => void
  onClick?: (ev: MouseEvent) => void
  alt?: string
  hideBroken?: boolean
}

const safeOrigins = [
  "data:image",
  "https://imgur.com/",
  "https://i.imgur.com/",
  "https://imgproxy.iris.to/",
]

const shouldSkipProxy = (url: string) => {
  return safeOrigins.some((origin) => url.startsWith(origin))
}

const ProxyImg = (props: Props) => {
  const [proxyFailed, setProxyFailed] = useState(false)
  const [src, setSrc] = useState(props.src)
  const [loadFailed, setLoadFailed] = useState(false)

  useEffect(() => {
    let mySrc = props.src
    if (
      props.src &&
      !props.src.startsWith("data:image") &&
      (!shouldSkipProxy(props.src) || props.width)
    ) {
      const originalSrc = props.src
      if (props.width) {
        const width = props.width * 2
        const resizeType = props.square ? "fill" : "fit"
        mySrc = `https://imgproxy.iris.to/insecure/rs:${resizeType}:${width}:${width}/plain/${originalSrc}`
      } else {
        mySrc = `https://imgproxy.iris.to/insecure/plain/${originalSrc}`
      }
      setSrc(mySrc)
    }
  }, [props.src, props.width, props.square])

  const handleError = () => {
    if (proxyFailed) {
      console.log("original source failed too", props.src)
      setLoadFailed(true)
      props.onError && props.onError()
      if (props.hideBroken) {
        setSrc("")
      }
    } else {
      console.log("image proxy failed", src, "trying original source", props.src)
      setProxyFailed(true)
      setSrc(props.src)
    }
  }

  if (!src || loadFailed) {
    return null
  }

  return (
    <img
      src={src}
      onError={handleError}
      onClick={props.onClick}
      className={props.className}
      style={props.style}
      alt={props.alt}
    />
  )
}

export default ProxyImg
