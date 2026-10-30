import type { GraphEntry } from './graph'
import { sourceFile } from './pipeline'
import type { ListNode } from './types'

/** Below this many nodes the nodes themselves fit on screen, and say more than a map. */
const MAP_FROM = 30
/** Past this many pipelines a map is noise too. */
const MAP_UP_TO = 60

/** Past this many edges a map is laid out as a grid, and draws only the selection's edges. */
const EDGES_UP_TO = 24

const PREFIX = 'domain:'

/** The source file a domain card stands for; null for any other id. */
export function domainFile(id: string): string | null {
  return id.startsWith(PREFIX) ? id.slice(PREFIX.length) : null
}

export interface DomainMap {
  entries: GraphEntry[]
  leaves: Map<string, string[]>
}

/**
 * The whole project as one card per pipeline, with one edge per pair of pipelines that depend
 * on each other, weighted by how many inputs cross it. Null when the map would not help: a
 * small project, a single file, or more files than cards can be read.
 */
export function domainMap(assets: ListNode[]): DomainMap | null {
  if (assets.length < MAP_FROM) return null
  const ids = new Set(assets.map((a) => a.id))
  const members = new Map<string, string[]>()
  const crossing = new Map<string, Record<string, number>>()
  for (const a of assets) {
    const file = PREFIX + sourceFile(a.id)
    members.set(file, [...(members.get(file) ?? []), a.id])
    for (const input of a.inputs) {
      const from = PREFIX + sourceFile(input)
      if (from === file || !ids.has(input)) continue
      const weights = crossing.get(file) ?? {}
      weights[from] = (weights[from] ?? 0) + 1
      crossing.set(file, weights)
    }
  }
  if (members.size < 2 || members.size > MAP_UP_TO) return null
  const feeds = new Map<string, number>()
  for (const weights of crossing.values())
    for (const from of Object.keys(weights)) feeds.set(from, (feeds.get(from) ?? 0) + 1)
  const edges = [...feeds.values()].reduce((sum, n) => sum + n, 0)
  const entries = [...members].map(([id, leaves]): GraphEntry => {
    const inputWeights = crossing.get(id) ?? {}
    const upstream = Object.keys(inputWeights).length
    return {
      id,
      kind: 'group',
      inputs: Object.keys(inputWeights),
      name: id.slice(PREFIX.length),
      summary: `${leaves.length.toLocaleString()} nodes · ${upstream} in · ${feeds.get(id) ?? 0} out`,
      inputWeights,
      loose: edges > EDGES_UP_TO,
    }
  })
  // Sources first: the grid reads in dependency order.
  entries.sort((a, b) => a.inputs.length - b.inputs.length)
  return { entries, leaves: members }
}
