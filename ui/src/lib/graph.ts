import dagre from '@dagrejs/dagre'
import { Position, type Node, type Edge } from '@xyflow/react'
import { DAG_NODE_WIDTH, DAG_NODE_HEIGHT } from '@/components'
import type { NodeKind, StatusKind } from './types'

export type LayoutDir = 'LR' | 'TB'

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

/**
 * Build positioned React Flow nodes + edges from the asset graph — purely
 * structural. Same input → same output; re-run only on structure or direction
 * change, never on a status tick (status is merged in afterward).
 */
export function buildGraph(assets: GraphEntry[], dir: LayoutDir) {
  const byId = new Map(assets.map((a) => [a.id, a]))

  const g = new dagre.graphlib.Graph()
  g.setGraph({ rankdir: dir, ranksep: assets.some(a => a.kind === 'group') ? 52 : 96, nodesep: 26, marginx: 36, marginy: 36 })
  g.setDefaultEdgeLabel(() => ({}))

  for (const a of assets) {
    g.setNode(a.id, { width: a.kind === 'group' ? 248 : DAG_NODE_WIDTH, height: a.kind === 'group' ? 112 : DAG_NODE_HEIGHT })
  }
  // Edge per dependency: input → asset.
  const deps: Array<{ source: string; target: string }> = []
  for (const a of assets) {
    for (const input of a.inputs) {
      if (byId.has(input)) {
        deps.push({ source: input, target: a.id })
        g.setEdge(input, a.id)
      }
    }
  }

  dagre.layout(g)

  const horizontal = dir === 'LR'
  const nodes: GraphNode[] = assets.map((a) => {
    const p = g.node(a.id)
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
      position: { x: p.x - (a.kind === 'group' ? 248 : DAG_NODE_WIDTH) / 2, y: p.y - (a.kind === 'group' ? 112 : DAG_NODE_HEIGHT) / 2 },
      sourcePosition: horizontal ? Position.Right : Position.Bottom,
      targetPosition: horizontal ? Position.Left : Position.Top,
    }
  })

  const edges: Edge[] = deps.map((e) => ({
    id: `${e.source}->${e.target}`,
    source: e.source,
    target: e.target,
    type: 'smoothstep',
  }))

  return { nodes, edges }
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
