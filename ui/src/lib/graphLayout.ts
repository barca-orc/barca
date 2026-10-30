import dagre from '@dagrejs/dagre'

/**
 * The layout of a graph, apart from React and React Flow, so that it can run in a worker
 * (`graphLayout.worker.ts`): dagre places boxes of the given sizes, rank by rank.
 */

export type LayoutDir = 'LR' | 'TB'

export interface LayoutInput {
  dir: LayoutDir
  ranksep: number
  /** `first` nodes go ahead of the rest when unconnected parts are packed. */
  nodes: { id: string; width: number; height: number; first?: boolean }[]
  /** `[source, target]`, both among `nodes`. */
  edges: [string, string][]
}

/** Each node's center, by id. */
export type LayoutPositions = Record<string, { x: number; y: number }>

/** The space between two unconnected parts of a graph. */
const GAP = 36
/** The shape unconnected parts are packed into: about that of a screen. */
const ASPECT = 2.2

/** A part of a graph laid out on its own: its size, and its nodes' centers within it. */
interface Part {
  width: number
  height: number
  first: boolean
  positions: LayoutPositions
}

function place(input: LayoutInput): Part {
  const g = new dagre.graphlib.Graph()
  g.setGraph({ rankdir: input.dir, ranksep: input.ranksep, nodesep: 26, marginx: 0, marginy: 0 })
  g.setDefaultEdgeLabel(() => ({}))
  for (const n of input.nodes) g.setNode(n.id, { width: n.width, height: n.height })
  for (const [source, target] of input.edges) g.setEdge(source, target)
  dagre.layout(g)
  const positions: LayoutPositions = {}
  for (const n of input.nodes) {
    const p = g.node(n.id)
    positions[n.id] = { x: p.x, y: p.y }
  }
  const { width = 0, height = 0 } = g.graph()
  return { width, height, first: input.nodes.some((n) => n.first), positions }
}

/** The weakly connected parts of a graph, in the order their first nodes appear. */
function connectedParts(input: LayoutInput): LayoutInput[] {
  const root = new Map(input.nodes.map((n) => [n.id, n.id]))
  const find = (id: string): string => {
    let at = id
    while (root.get(at) !== at) at = root.get(at)!
    root.set(id, at)
    return at
  }
  for (const [source, target] of input.edges) root.set(find(source), find(target))
  const parts = new Map<string, LayoutInput>()
  for (const n of input.nodes) {
    const key = find(n.id)
    const part = parts.get(key) ?? { ...input, nodes: [], edges: [] }
    part.nodes.push(n)
    parts.set(key, part)
  }
  for (const edge of input.edges) parts.get(find(edge[0]))!.edges.push(edge)
  return [...parts.values()]
}

/**
 * Each connected part by dagre, then the parts packed row by row, biggest first: dagre alone
 * stacks nodes with no edges between them in one tall column, which no zoom makes readable.
 */
export function layout(input: LayoutInput): LayoutPositions {
  const parts = connectedParts(input).map(place)
  parts.sort((a, b) => Number(b.first) - Number(a.first) || b.height * b.width - a.height * a.width)
  const area = parts.reduce((sum, p) => sum + (p.width + GAP) * (p.height + GAP), 0)
  const rowWidth = Math.max(Math.sqrt(area * ASPECT), ...parts.map((p) => p.width))
  const out: LayoutPositions = {}
  let x = 0
  let y = 0
  let rowHeight = 0
  for (const part of parts) {
    if (x > 0 && x + part.width > rowWidth) {
      x = 0
      y += rowHeight + GAP
      rowHeight = 0
    }
    for (const [id, p] of Object.entries(part.positions)) out[id] = { x: p.x + x, y: p.y + y }
    x += part.width + GAP
    rowHeight = Math.max(rowHeight, part.height)
  }
  return out
}
