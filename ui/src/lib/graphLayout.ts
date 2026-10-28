import dagre from '@dagrejs/dagre'

/**
 * The layout of a graph, apart from React and React Flow, so that it can run in a worker
 * (`graphLayout.worker.ts`): dagre places boxes of the given sizes, rank by rank.
 */

export type LayoutDir = 'LR' | 'TB'

export interface LayoutInput {
  dir: LayoutDir
  ranksep: number
  nodes: { id: string; width: number; height: number }[]
  /** `[source, target]`, both among `nodes`. */
  edges: [string, string][]
}

/** Each node's center, by id. */
export type LayoutPositions = Record<string, { x: number; y: number }>

export function layout(input: LayoutInput): LayoutPositions {
  const g = new dagre.graphlib.Graph()
  g.setGraph({ rankdir: input.dir, ranksep: input.ranksep, nodesep: 26, marginx: 36, marginy: 36 })
  g.setDefaultEdgeLabel(() => ({}))
  for (const n of input.nodes) g.setNode(n.id, { width: n.width, height: n.height })
  for (const [source, target] of input.edges) g.setEdge(source, target)
  dagre.layout(g)
  const out: LayoutPositions = {}
  for (const n of input.nodes) {
    const p = g.node(n.id)
    out[n.id] = { x: p.x, y: p.y }
  }
  return out
}
