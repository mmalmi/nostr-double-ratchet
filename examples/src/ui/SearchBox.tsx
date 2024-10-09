import React, {useEffect, useRef, useState, useCallback} from "react"
import {nip19} from "nostr-tools"
import classNames from "classnames"
import {SearchIcon} from "./Icons"
import { Avatar } from "./Avatar"
import { debounce } from "lodash"
import FollowedBy from "./FollowedBy"
import fuse from "../utils/fuse"
import socialGraph from "../utils/socialGraph"

const NOSTR_REGEX = /(npub|note|nevent)1[a-zA-Z0-9]{58,300}/gi
const HEX_REGEX = /[0-9a-fA-F]{64}/gi
const MAX_RESULTS = 10
const NIP05_REGEX = /^[^\s@]+@[^\s@]+\.[^\s@]+$/

function SearchBox({onSelect}: {onSelect?: (string) => void }) {
  const [searchResults, setSearchResults] = useState([])
  const [activeResult, setActiveResult] = useState(0)
  const [value, setValue] = useState("")
  const inputRef = useRef(null)

  const debouncedSearch = useCallback(
    debounce((v) => {
      if (v.match(NOSTR_REGEX)) {
        setValue("")
        const hex = nip19.decode(v)
        onSelect?.(hex.data)
        return
      }

      if (v.match(HEX_REGEX)) {
        setValue("")
        onSelect?.(v)
        return
      }

      const results = fuse.search(v, { limit: MAX_RESULTS })
      console.log('results', results)

      // Fetch follow distances and adjust scores
      const resultsWithAdjustedScores = results.map((result) => {
        const followDistance = Math.max(socialGraph().getFollowDistance(result.item.pubKey), 1)
        const adjustedScore = result.score! * followDistance * Math.pow(0.999, socialGraph().followedByFriends(result.item.pubKey).size)
        return { ...result, adjustedScore }
      })

      resultsWithAdjustedScores.sort((a, b) => a.adjustedScore - b.adjustedScore)

      setActiveResult(0)
      setSearchResults(resultsWithAdjustedScores.map((result) => result.item))
    }, 100),
    []
  )

  useEffect(() => {
    const v = value.trim()
    if (v) {
      debouncedSearch(v)
    } else {
      setSearchResults([])
    }
  }, [value, debouncedSearch])

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!value) return

      if (e.key === "ArrowDown") {
        e.preventDefault()
        setActiveResult((prev) => (prev + 1) % MAX_RESULTS)
      } else if (e.key === "ArrowUp") {
        e.preventDefault()
        setActiveResult((prev) => (prev - 1 + MAX_RESULTS) % MAX_RESULTS)
      } else if (e.key === "Escape") {
        setValue("")
        setSearchResults([])
      } else if (e.key === "Enter" && searchResults.length > 0) {
        const activeItem = searchResults[activeResult]
        onSelect?.(activeItem.pubKey)
        setValue("")
        setSearchResults([])
      }
    }
    window.addEventListener("keydown", handleKeyDown)
    return () => window.removeEventListener("keydown", handleKeyDown)
  }, [searchResults, activeResult])

  const handleSearchResultClick = (pubKey: string) => {
    onSelect?.(pubKey)
    setValue("")
    setSearchResults([])
  }

  const highlightMatch = (text: string, query: string) => {
    const parts = text.split(new RegExp(`(${query})`, 'gi'))
    return parts.map((part, index) =>
      part.toLowerCase() === query.toLowerCase() ? <b key={index}>{part}</b> : part
    )
  }

  return (
    <div className={"dropdown dropdown-open"}>
      <label className="input input-bordered rounded-full flex items-center gap-2">
        <input
          type="text"
          className="grow"
          placeholder="Search (or paste public key)"
          value={value}
          ref={inputRef}
          onChange={(e) => setValue(e.target.value)}
        />
        <SearchIcon />
      </label>
      {searchResults.length > 0 && (
        <ul className="dropdown-content menu shadow bg-base-100 rounded-box z-10 w-full mt-1">
          {searchResults.slice(0, MAX_RESULTS).map((result, index) => result && (
            <li
              key={result.pubKey}
              className={classNames("cursor-pointer rounded-md", {
                "bg-base-300": index === activeResult,
                "hover:bg-base-200": index !== activeResult,
              })}
              onClick={() => handleSearchResultClick(result.pubKey)}
            >
              <div className="flex flex-col justify-start items-start p-4">
                <div className="flex gap-2 items-center">
                  <Avatar showBadge={false} pubKey={result.pubKey} />
                  <span>
                      {highlightMatch(result.name, value)}
                  </span>
                </div>
                <FollowedBy pubkey={result.pubKey} />
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

export default SearchBox
