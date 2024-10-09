import Fuse from "fuse.js"
// pre-generated index can be used, but not significant performance improvement if dataset not super large
// import fuseIndexData from "../../data/profileIndex.json"
import fuseData from "../../../data/profileData.json"

type SearchResult = {
    query?: string
    name: string
    pubKey: string
    nip05?: string
  }

console.time('fuse init')
// const fuseIndex = Fuse.parseIndex(fuseIndexData)
const processedData = fuseData.map((v) => ({ pubKey: v[0], name: v[1], nip05: v[2] || undefined }));
const fuse = new Fuse<SearchResult>(processedData as SearchResult[], { keys: ["name", "pubKey"], includeScore: true })
console.timeEnd('fuse init')
console.log(fuse)

export default fuse