import dagre from '@dagrejs/dagre'

/**
 * The layout of a graph, apart from React and React Flow, so that it can run in a worker
 * (`graphLayout.worker.ts`): dagre places boxes of the given sizes, rank by rank, and routes each
 * edge between them so that it never crosses a box.
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

export interface Point {
  x: number
  y: number
}

/** Each node's center, by id. */
export type LayoutPositions = Record<string, Point>

/**
 * Where nodes go and how edges run: `routes` by edge id (`source->target`), from the source's
 * outgoing side to the target's incoming side. Consecutive points with the same cross-axis
 * coordinate are a straight run (through a rank, between its boxes); others are joined by an
 * S-curve, which only ever happens in the empty space between two ranks.
 */
export interface Layout {
  positions: LayoutPositions
  routes: Record<string, Point[]>
}

/** The id of the edge from `source` to `target`, as React Flow and `routes` know it. */
export function edgeId(source: string, target: string): string {
  return `${source}->${target}`
}

/** The space between two unconnected parts of a graph. */
const GAP = 36
/** The shape unconnected parts are packed into: about that of a screen. */
const ASPECT = 2.2
/** Between two boxes of a rank; an edge passing between them keeps half of it plus half of EDGE_SEP from each. */
const NODE_SEP = 32
/** Between two edges running side by side through a rank. */
const EDGE_SEP = 14

/** A part of a graph laid out on its own: its size, and its nodes' centers within it. */
interface Part {
  width: number
  height: number
  first: boolean
  layout: Layout
}

/** The coordinate along the ranks (`along`) and across them, for a direction. */
function axes(dir: LayoutDir) {
  const lr = dir === 'LR'
  return {
    along: (p: Point) => (lr ? p.x : p.y),
    across: (p: Point) => (lr ? p.y : p.x),
    point: (along: number, across: number): Point => (lr ? { x: along, y: across } : { x: across, y: along }),
    /** Half a box's extent along the ranks. */
    half: (box: { width: number; height: number }) => (lr ? box.width : box.height) / 2,
  }
}

function place(input: LayoutInput): Part {
  const g = new dagre.graphlib.Graph()
  g.setGraph({ rankdir: input.dir, ranksep: input.ranksep, nodesep: NODE_SEP, edgesep: EDGE_SEP, marginx: 0, marginy: 0 })
  g.setDefaultEdgeLabel(() => ({}))
  for (const n of input.nodes) g.setNode(n.id, { width: n.width, height: n.height })
  for (const [source, target] of input.edges) g.setEdge(source, target)
  dagre.layout(g)
  const positions: LayoutPositions = {}
  for (const n of input.nodes) {
    const p = g.node(n.id)
    positions[n.id] = { x: p.x, y: p.y }
  }
  const routes = routeEdges(input, positions, (source, target) => g.edge(source, target)?.points ?? [])
  const { width = 0, height = 0 } = g.graph()
  return { width, height, first: input.nodes.some((n) => n.first), layout: { positions, routes } }
}

/**
 * Each edge's route from dagre's points: out of the source's side, straight to the end of its
 * rank; through every rank it passes on the line dagre kept clear between that rank's boxes;
 * into the target's side. The bends are all in the gaps between ranks, where no box is.
 */
export function routeEdges(
  input: LayoutInput,
  positions: LayoutPositions,
  pointsOf: (source: string, target: string) => readonly Point[],
): Record<string, Point[]> {
  const { along, across, point, half } = axes(input.dir)
  const sizes = new Map(input.nodes.map((n) => [n.id, n]))
  // Each rank by where its centers are along the ranks, and half its widest box.
  const ranks: { at: number; half: number }[] = []
  for (const n of input.nodes) {
    const at = along(positions[n.id]!)
    const rank = ranks.find((r) => Math.abs(r.at - at) < 0.5)
    if (rank) rank.half = Math.max(rank.half, half(n))
    else ranks.push({ at, half: half(n) })
  }
  const rankAt = (at: number) => ranks.find((r) => Math.abs(r.at - at) < 0.5)
  const routes: Record<string, Point[]> = {}
  for (const [source, target] of input.edges) {
    const from = positions[source]!
    const to = positions[target]!
    const start = point(along(from) + half(sizes.get(source)!), across(from))
    const end = point(along(to) - half(sizes.get(target)!), across(to))
    const route = [start]
    const out = rankAt(along(from))
    if (out && out.half > half(sizes.get(source)!)) route.push(point(out.at + out.half, across(from)))
    for (const p of pointsOf(source, target).slice(1, -1)) {
      const rank = rankAt(along(p))
      if (rank) route.push(point(rank.at - rank.half, across(p)), point(rank.at + rank.half, across(p)))
      else route.push(p)
    }
    const into = rankAt(along(to))
    if (into && into.half > half(sizes.get(target)!)) route.push(point(into.at - into.half, across(to)))
    route.push(end)
    routes[edgeId(source, target)] = route
  }
  return routes
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
 * Each connected part by dagre, by dependency depth, then the parts packed row by row, biggest
 * first: dagre alone stacks nodes with no edges between them in one tall column, which no zoom
 * makes readable. Nodes with no dependencies are packed compactly; those with any stay ranked.
 */
export function layout(input: LayoutInput): Layout {
  const parts = connectedParts(input).map(place)
  parts.sort((a, b) => Number(b.first) - Number(a.first) || b.height * b.width - a.height * a.width)
  const area = parts.reduce((sum, p) => sum + (p.width + GAP) * (p.height + GAP), 0)
  const rowWidth = Math.max(Math.sqrt(area * ASPECT), ...parts.map((p) => p.width))
  const out: Layout = { positions: {}, routes: {} }
  let x = 0
  let y = 0
  let rowHeight = 0
  for (const part of parts) {
    if (x > 0 && x + part.width > rowWidth) {
      x = 0
      y += rowHeight + GAP
      rowHeight = 0
    }
    for (const [id, p] of Object.entries(part.layout.positions)) out.positions[id] = { x: p.x + x, y: p.y + y }
    for (const [id, route] of Object.entries(part.layout.routes)) out.routes[id] = route.map((p) => ({ x: p.x + x, y: p.y + y }))
    x += part.width + GAP
    rowHeight = Math.max(rowHeight, part.height)
  }
  return out
}

/**
 * An SVG path through a route: straight where two points share the cross-axis coordinate, an
 * S-curve (leaving and arriving along the ranks) where they do not.
 */
export function routePath(route: readonly Point[], dir: LayoutDir): string {
  if (route.length === 0) return ''
  const { along, across, point } = axes(dir)
  const fmt = (p: Point) => `${round(p.x)},${round(p.y)}`
  let d = `M${fmt(route[0]!)}`
  for (let i = 1; i < route.length; i++) {
    const a = route[i - 1]!
    const b = route[i]!
    if (Math.abs(across(a) - across(b)) < 0.5) d += ` L${fmt(b)}`
    else {
      const mid = (along(a) + along(b)) / 2
      d += ` C${fmt(point(mid, across(a)))} ${fmt(point(mid, across(b)))} ${fmt(b)}`
    }
  }
  return d
}

/** Where a route's label goes: halfway along its middle step. */
export function routeMiddle(route: readonly Point[]): Point {
  if (route.length === 0) return { x: 0, y: 0 }
  const i = Math.max(0, Math.floor((route.length - 1) / 2))
  const a = route[i]!
  const b = route[Math.min(i + 1, route.length - 1)]!
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }
}

function round(n: number): number {
  return Math.round(n * 10) / 10
}
