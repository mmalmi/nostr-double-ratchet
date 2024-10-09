import { SerializedSocialGraph, SocialGraph } from "../../../src"
import throttle from "lodash/throttle"
import localForage from "localforage"

export const LOCALSTORAGE_PUBLICKEY = "iris.search.currentUser"
export const DEFAULT_SOCIAL_GRAPH_ROOT =
  "4523be58d395b1b196a9b8c82b038b6895cb02b683d0c253a955068dba1facd0"
const LOCALFORAGE_KEY = "iris.socialGraph"

let publicKey = DEFAULT_SOCIAL_GRAPH_ROOT
try {
  const k = localStorage.getItem(LOCALSTORAGE_PUBLICKEY)
  if (k) {
    publicKey = k
  }
} catch (e) {
  //
}

let graph: SocialGraph

export const saveGraph = throttle(async () => {
  try {
    const data = graph.serialize()
    await localForage.setItem(LOCALFORAGE_KEY, JSON.stringify(data))
  } catch (e) {
    console.error("Error saving graph", e)
  }
}, 5000)

const initGraph = async () => {
  try {
    const data = await localForage.getItem(LOCALFORAGE_KEY)
    if (typeof data === 'string' && data) {
      graph = new SocialGraph(publicKey, JSON.parse(data) as SerializedSocialGraph)
    }
  } catch (e) {
    console.error('Error loading graph')
    localForage.removeItem(LOCALFORAGE_KEY)
  }
  if (!graph) {
    const { default: socialGraphData } = await import("../../../data/socialGraph.json")
    graph = new SocialGraph(publicKey, socialGraphData as unknown as SerializedSocialGraph)
  }
}

export const saveToFile = () => {
  const data = graph.serialize()
  const url = URL.createObjectURL(
    new File([JSON.stringify(data)], "social_graph.json", {
      type: "text/json",
    }),
  );
  const a = document.createElement("a");
  a.href = url;
  a.download = "social_graph.json";
  a.click();
}

export const loadFromFile = (merge = false) => {
  const input = document.createElement("input")
  input.type = "file"
  input.accept = ".json"
  input.multiple = false
  input.onchange = () => {
    if (input.files?.length) {
      const file = input.files[0]
      file.text().then((json) => {
        try {
          const data = JSON.parse(json)
          if (merge) {
            graph.merge(new SocialGraph(graph.getRoot(), data))
          } else {
            graph = new SocialGraph(graph.getRoot(), data)
          }
        } catch (e) {
          console.error("failed to load social graph from file:", e)
        }
      })
    }
  }
  input.click()
}

export const loadAndMerge = () => loadFromFile(true)

export const socialGraphLoaded = new Promise(async (resolve) => {
  await initGraph()
  resolve(true)
})

export default () => graph