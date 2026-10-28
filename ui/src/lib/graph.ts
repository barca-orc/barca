import { Position, type Node, type Edge } from '@xyflow/react'
import { DAG_NODE_WIDTH, DAG_NODE_HEIGHT } from '@/components'
import { layout, type LayoutDir, type LayoutInput, type LayoutPositions } from './graphLayout'
import type { NodeKind, StatusKind } from './types'

export type { LayoutDir }

/** Data carried on each React Flow node (consumed by the custom AssetNode). */
export interface GraphNodeData {
  id: string
  name: string
  kind: NodeKind | 'group'
  summary?: string
  outputName?: string
  status: StatusKind
  metric?: string | null
  stateHint?: string
  [key: string]: unknown
}

export type GraphNode = Node<GraphNodeData>

/** Organizational entries can be laid out alongside real pipeline nodes. */
export interface GraphEntry {
  id: string
  kind: NodeKind | 'group'
  inputs: string[]
  name?: string
  summary?: string
  outputName?: string
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

function size(a: GraphEntry) {
  return a.kind === 'group'
    ? { width: GROUP_WIDTH, height: GROUP_HEIGHT }
    : { width: DAG_NODE_WIDTH, height: DAG_NODE_HEIGHT }
}

/** What dagre is given for `assets`: their sizes and the dependencies between them. */
export function layoutInput(assets: GraphEntry[], dir: LayoutDir): LayoutInput {
  const ids = new Set(assets.map((a) => a.id))
  return {
    dir,
    ranksep: assets.some((a) => a.kind === 'group') ? 52 : 96,
    nodes: assets.map((a) => ({ id: a.id, ...size(a) })),
    // Edge per dependency: input → asset.
    edges: assets.flatMap((a) =>
      a.inputs.filter((input) => ids.has(input)).map((input): [string, string] => [input, a.id]),
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
      type: a.kind === 'group' ? 'nodeGroup' : 'asset',
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
  const edges: Edge[] = layoutInput(assets, dir).edges.map(([source, target]) => ({
    id: `${source}->${target}`,
    source,
    target,
    type: 'smoothstep',
  }))
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
