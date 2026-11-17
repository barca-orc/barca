import { Position, type Node, type Edge } from '@xyflow/react'
import { match } from 'ts-pattern'
import { DAG_NODE_WIDTH, DAG_NODE_HEIGHT } from '@/components'
import { edgeId, layout, type Layout, type LayoutDir, type LayoutInput, type Point } from './graphLayout'
import type { Tone } from './tone'
import type { Glyph } from './glyph'
import type { NodeKind, StatusKind } from './types'

export type { LayoutDir }

/** Data carried on each React Flow node (consumed by the custom AssetNode). */
interface GraphNodeData {
  id: string
  name: string
  kind: GraphKind
  summary?: string
  outputName?: string
  /** How urgent its state is; `glyph` draws it and `label` is the word. */
  tone: Tone
  /** How its state is drawn; none before any state is known. */
  glyph?: Glyph
  /** The state's word, or a group's summary. */
  label?: string | null
  /** The cause (or fresh) its resting state is, drawn in the health map's colour; none for run work or before any state. */
  part?: import('./causeDisplay').MapPart
  /** Its last attempt failed: the failures a page lists read it. */
  lastFailed?: boolean
  /** Pinned (`barca pin`): what it is pinned to; it reads `pinned`. */
  pinned?: string
  /** Whether the node pulses: a run is working on it. */
  live?: boolean
  stateHint?: string
  /** A partitioned node's keys: drawn as a count and a bar of the failed and waiting ones. */
  keys?: import('./graphState').GraphKeys
  /** A group's health, most severe first: the card leads with the first, the rest compactly. */
  parts?: readonly import('./graphState').GroupPart[]
  [key: string]: unknown
}

export type GraphNode = Node<GraphNodeData>

/** What a box in the graph is: a node, a group of them, or an input from outside the view. */
type GraphKind = NodeKind | 'group' | 'port'

/** Organizational entries can be laid out alongside real pipeline nodes. */
export interface GraphEntry {
  id: string
  kind: GraphKind
  inputs: string[]
  name?: string
  summary?: string
  outputName?: string
  /** How many dependencies an edge from an input stands for, when more than one. */
  inputWeights?: Record<string, number>
  /** Placed apart from its edges: they would pull it into a column, or a chain off the screen. */
  loose?: boolean
}

/** Data carried on each edge. */
export interface GraphEdgeData {
  weight?: number
  /** An edge from an input outside the view. */
  port: boolean
  /** From a loose entry: drawn only for the selection. */
  loose: boolean
  /**
   * In a graph of groups, a dependency another path already shows (a → c beside a → b → c):
   * left out of the layout and drawn only for the selection or the hovered card.
   */
  implied?: boolean
  /** Its route around the boxes (`layout`); none for a loose edge, which is drawn as a plain curve. */
  route?: Point[]
  direction: LayoutDir
  [key: string]: unknown
}

/** Short name: the part after the last colon in a node id. */
export function shortName(id: string): string {
  const idx = id.lastIndexOf(':')
  return idx === -1 ? id : id.slice(idx + 1)
}

/**
 * Resting node tone before any state. Static analysis (`GET /assets`) carries
 * no state — cache state and active run status are overlaid downstream
 * (see GraphCanvas + `overlayRunStatus`), never decided here.
 */
const RESTING_TONE: Tone = 'neutral'

const GROUP_WIDTH = 248
const GROUP_HEIGHT = 112

const PORT_WIDTH = 176
const PORT_HEIGHT = 28

function size(a: GraphEntry) {
  return match(a.kind)
    .with('group', () => ({ width: GROUP_WIDTH, height: GROUP_HEIGHT }))
    .with('port', () => ({ width: PORT_WIDTH, height: PORT_HEIGHT }))
    .with('asset', 'sensor', 'task', () => ({ width: DAG_NODE_WIDTH, height: DAG_NODE_HEIGHT }))
    .exhaustive()
}

function nodeType(kind: GraphKind): string {
  return match(kind)
    .with('group', () => 'nodeGroup')
    .with('port', () => 'port')
    .with('asset', 'sensor', 'task', () => 'asset')
    .exhaustive()
}

/** Edge per dependency: input → asset. */
function dependencies(assets: readonly GraphEntry[]): [string, string][] {
  const ids = new Set(assets.map((a) => a.id))
  return assets.flatMap((a) =>
    a.inputs.filter((input) => ids.has(input)).map((input): [string, string] => [input, a.id]),
  )
}

/** Past this many entries, implied edges are not looked for: it costs a walk per edge. */
const IMPLIED_UP_TO = 400

/**
 * The edges of a graph of groups that another path already shows: `a → c` when `a → b → c`.
 * A summary of groups is mostly such edges (a domain that feeds everything downstream), and
 * drawing them all is a tangle; the remaining ones (its transitive reduction) keep every
 * dependency readable as a path. Empty for a graph of nodes, whose every edge is a real input,
 * and past `IMPLIED_UP_TO` entries.
 */
export function impliedEdges(assets: readonly GraphEntry[]): Set<string> {
  const out = new Set<string>()
  if (!assets.some((a) => a.kind === 'group') || assets.length > IMPLIED_UP_TO) return out
  const edges = dependencies(assets).filter(([source, target]) => !loosePair(assets, source, target))
  const next = new Map<string, string[]>()
  for (const [source, target] of edges) next.set(source, [...(next.get(source) ?? []), target])
  for (const [source, target] of edges) {
    // Is `target` reachable from `source` without this edge?
    const seen = new Set<string>()
    const stack = (next.get(source) ?? []).filter((n) => n !== target)
    let found = false
    while (stack.length > 0 && !found) {
      const at = stack.pop()!
      if (seen.has(at)) continue
      seen.add(at)
      for (const n of next.get(at) ?? []) {
        if (n === target) found = true
        else stack.push(n)
      }
    }
    if (found) out.add(edgeId(source, target))
  }
  return out
}

function loosePair(assets: readonly GraphEntry[], source: string, target: string): boolean {
  return !!assets.find((a) => a.id === source)?.loose || !!assets.find((a) => a.id === target)?.loose
}

/** What dagre is given for `assets`: their sizes and the dependencies between them. */
export function layoutInput(assets: GraphEntry[], dir: LayoutDir): LayoutInput {
  const byId = new Map(assets.map((a) => [a.id, a]))
  const implied = impliedEdges(assets)
  return {
    dir,
    // Room between ranks for the edges' bends: they never cross a box, so they turn here.
    ranksep: assets.some((a) => a.kind === 'group') ? 72 : 80,
    nodes: assets.map((a) => ({ id: a.id, ...size(a), first: a.kind === 'port' })),
    edges: dependencies(assets).filter(
      ([source, target]) => !byId.get(source)?.loose && !byId.get(target)?.loose && !implied.has(edgeId(source, target)),
    ),
  }
}

/**
 * React Flow nodes + edges for `assets` at `laidOut` (from `layout` of their `layoutInput`).
 * Purely structural: status is merged in afterward.
 */
export function graphAt(assets: GraphEntry[], dir: LayoutDir, laidOut: Layout) {
  const { positions, routes } = laidOut
  const horizontal = dir === 'LR'
  const nodes: GraphNode[] = assets.map((a) => {
    const p = positions[a.id] ?? { x: 0, y: 0 }
    const { width, height } = size(a)
    return {
      id: a.id,
      type: nodeType(a.kind),
      selectable: a.kind !== 'port',
      // The minimap skips a node with no size of its own, and nothing here writes back React Flow's measurements.
      initialWidth: width,
      initialHeight: height,
      data: {
        id: a.id,
        direction: dir,
        name: a.name ?? shortName(a.id),
        kind: a.kind,
        summary: a.summary,
        outputName: a.outputName,
        tone: RESTING_TONE,
      },
      // dagre centers nodes; React Flow positions by top-left.
      position: { x: p.x - width / 2, y: p.y - height / 2 },
      sourcePosition: horizontal ? Position.Right : Position.Bottom,
      targetPosition: horizontal ? Position.Left : Position.Top,
    }
  })
  const byId = new Map(assets.map((a) => [a.id, a]))
  const implied = impliedEdges(assets)
  const edges: Edge<GraphEdgeData>[] = dependencies(assets).map(([source, target]) => {
    const weight = byId.get(target)?.inputWeights?.[source]
    const id = edgeId(source, target)
    const route = routes[id]
    return {
      id,
      source,
      target,
      type: 'routed',
      data: { weight, port: byId.get(source)?.kind === 'port', loose: !!byId.get(source)?.loose, direction: dir, ...(implied.has(id) ? { implied: true } : {}), ...(route ? { route } : {}) },
      // Wider with weight, but no edge may dwarf a card.
      style: weight ? { strokeWidth: Math.min(1 + Math.log2(weight) / 2, 3.5) } : undefined,
    }
  })
  return { nodes, edges }
}

/**
 * Build positioned React Flow nodes + edges from the asset graph — purely
 * structural. Same input → same output; re-run only on structure or direction
 * change, never on a status tick (status is merged in afterward).
 */
export function buildGraph(assets: GraphEntry[], dir: LayoutDir) {
  return graphAt(assets, dir, layout(layoutInput(assets, dir)))
}

/**
 * Overlay an optimistic "running" status onto the triggered node so the click
 * feels instant before the first event lands. Pure — given the stream-derived
 * statuses, returns the map the graph should render.
 */
export function overlayRunStatus(
  statuses: Record<string, StatusKind>,
  running: boolean,
  nodeId: string | null,
): Record<string, StatusKind> {
  if (running && nodeId && !statuses[nodeId]) {
    return { ...statuses, [nodeId]: 'running' }
  }
  return statuses
}

/**
 * Classify an edge from the tones of its endpoints. `live` (cyan, flowing) means a fresh
 * (cached, or just succeeded) upstream is feeding a running downstream; `hot` means the edge
 * touches the selected node. Pure → testable; rendered via CSS class.
 */
export function edgeClassName(opts: {
  sourceTone?: Tone
  targetTone?: Tone
  hot: boolean
}): string {
  const live = opts.sourceTone === 'success' && opts.targetTone === 'running'
  return [live ? 'live' : '', opts.hot ? 'hot' : ''].filter(Boolean).join(' ')
}
