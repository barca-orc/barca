import { Position, type Node, type Edge } from '@xyflow/react'
import { match } from 'ts-pattern'
import { DAG_NODE_WIDTH, DAG_NODE_HEIGHT } from '@/components'
import { layout, type LayoutDir, type LayoutInput, type LayoutPositions } from './graphLayout'
import type { NodeKind, StatusKind } from './types'

export type { LayoutDir }

/** Data carried on each React Flow node (consumed by the custom AssetNode). */
export interface GraphNodeData {
  id: string
  name: string
  kind: GraphKind
  summary?: string
  outputName?: string
  status: StatusKind
  metric?: string | null
  stateHint?: string
  [key: string]: unknown
}

export type GraphNode = Node<GraphNodeData>

/** What a box in the graph is: a node, a group of them, or an input from outside the view. */
export type GraphKind = NodeKind | 'group' | 'port'

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
  [key: string]: unknown
}

/** Short name: the part after the last colon in a node id. */
export function shortName(id: string): string {
  const idx = id.lastIndexOf(':')
  return idx === -1 ? id : id.slice(idx + 1)
}

/**
 * Resting node status before any run. Static analysis (`GET /assets`) carries
 * no state — cache state and active run status are overlaid downstream
 * (see GraphCanvas + `overlayRunStatus`), never decided here.
 */
const RESTING_STATUS: StatusKind = 'skipped'

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
function dependencies(assets: GraphEntry[]): [string, string][] {
  const ids = new Set(assets.map((a) => a.id))
  return assets.flatMap((a) =>
    a.inputs.filter((input) => ids.has(input)).map((input): [string, string] => [input, a.id]),
  )
}

/** What dagre is given for `assets`: their sizes and the dependencies between them. */
export function layoutInput(assets: GraphEntry[], dir: LayoutDir): LayoutInput {
  const byId = new Map(assets.map((a) => [a.id, a]))
  return {
    dir,
    ranksep: assets.some((a) => a.kind === 'group') ? 52 : 96,
    nodes: assets.map((a) => ({ id: a.id, ...size(a), first: a.kind === 'port' })),
    edges: dependencies(assets).filter(
      ([source, target]) => !byId.get(source)?.loose && !byId.get(target)?.loose,
    ),
  }
}

/**
 * React Flow nodes + edges for `assets` at `positions` (from `layout` of their `layoutInput`).
 * Purely structural: status is merged in afterward.
 */
export function graphAt(assets: GraphEntry[], dir: LayoutDir, positions: LayoutPositions) {
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
        status: RESTING_STATUS,
      },
      // dagre centers nodes; React Flow positions by top-left.
      position: { x: p.x - width / 2, y: p.y - height / 2 },
      sourcePosition: horizontal ? Position.Right : Position.Bottom,
      targetPosition: horizontal ? Position.Left : Position.Top,
    }
  })
  const byId = new Map(assets.map((a) => [a.id, a]))
  const edges: Edge<GraphEdgeData>[] = dependencies(assets).map(([source, target]) => {
    const weight = byId.get(target)?.inputWeights?.[source]
    return {
      id: `${source}->${target}`,
      source,
      target,
      type: 'smoothstep',
      data: { weight, port: byId.get(source)?.kind === 'port', loose: !!byId.get(source)?.loose },
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
 * Classify an edge from the *overlaid* statuses of its endpoints. `live` (cyan,
 * flowing) means a fresh upstream is feeding a running downstream; `hot` means
 * the edge touches the selected node. Pure → testable; rendered via CSS class.
 */
export function edgeClassName(opts: {
  sourceStatus?: StatusKind
  targetStatus?: StatusKind
  hot: boolean
}): string {
  const live = opts.sourceStatus === 'success' && opts.targetStatus === 'running'
  return [live ? 'live' : '', opts.hot ? 'hot' : ''].filter(Boolean).join(' ')
}
