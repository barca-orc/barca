import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, type KeyboardEvent, type Ref } from 'react'
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  useReactFlow,
  useStore,
  type Edge,
  type Node,
  type NodeTypes,
} from '@xyflow/react'
import { AssetNode } from './AssetNode'
import { GroupNode } from './GroupNode'
import { PortNode } from './PortNode'
import { blockedLabels, fitTarget, isArrowKey, neighborInDirection, READABLE_ZOOM, type Inset, type ViewNode } from './graphView'
import { PENDING_GRAPH_STATE, UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { edgeClassName, type LayoutDir, type GraphEdgeData, type GraphNode, type GraphEntry } from '@/lib/graph'
import { domainFile } from '@/lib/domainMap'
import { useGraphLayout } from '@/hooks/useGraphLayout'
import { statusMeta } from '@/lib/status'
import type { StatusKind } from '@/lib/types'

// Defined once, outside the component — a fresh object each render is a perf bug.
const nodeTypes: NodeTypes = { asset: AssetNode, nodeGroup: GroupNode, port: PortNode }

/** Room around a fit for what floats over the canvas: the legend, and the minimap beside it. */
const FIT_INSET: Inset = { top: 32, right: 32, bottom: 64, left: 56 }
const FIT_INSET_BESIDE_MINIMAP: Inset = { ...FIT_INSET, right: 236 }
const FIT_DURATION = 200

/** Below this many nodes the whole graph fits on screen, and a minimap only covers it. */
const MINIMAP_FROM = 60
/**
 * The minimap draws every node again on each pan and zoom: above this many it costs more than
 * it shows (half the main-thread time of a pan at a thousand nodes), and is left out.
 */
const MINIMAP_UP_TO = 400

/** Up to this many edges each shows what it stands for; past it, only the selection's do. */
const LABEL_EDGES_UP_TO = 16

const NO_GRAPH: { nodes: GraphNode[]; edges: Edge[] } = { nodes: [], edges: [] }

export interface GraphCanvasHandle {
  fit: () => void
}

interface GraphCanvasProps {
  assets: GraphEntry[]
  dir: LayoutDir
  selected: string | null
  onSelect: (id: string | null) => void
  onOpenGroup?: (id: string) => void
  /** Persistent state with active-run overlays. */
  states: Record<string, GraphState>
  handleRef?: Ref<GraphCanvasHandle>
}

function viewNodes(nodes: GraphNode[]): ViewNode[] {
  return nodes.map((n) => ({
    id: n.id,
    x: n.position.x,
    y: n.position.y,
    width: n.measured?.width ?? n.initialWidth ?? 0,
    height: n.measured?.height ?? n.initialHeight ?? 0,
    status: n.data.status,
    inert: n.type === 'port',
  }))
}

/**
 * Fit the view once custom nodes are measured, and re-fit whenever the layout
 * direction flips. React Flow can't fit unmeasured nodes, so the `fitView` prop
 * alone races the first paint of DOM-rendered custom nodes.
 *
 * A fit never goes below the readable zoom (1:1): a graph that fits is shown whole at that
 * size, a bigger one from its selected node, else its failed nodes (`fitTarget`).
 */
function FitController({ nodes, layoutKey, focus, minimap, handleRef }: { nodes: GraphNode[]; layoutKey: string; focus: string | null; minimap: boolean; handleRef?: Ref<GraphCanvasHandle> }) {
  const { fitView } = useReactFlow()
  const width = useStore((s) => s.width)
  const height = useStore((s) => s.height)
  const inset = minimap ? FIT_INSET_BESIDE_MINIMAP : FIT_INSET
  // Read when the layout changes: selecting a node, or a status tick, does not move the view.
  const latest = useRef({ nodes, focus, width, height, inset })
  useEffect(() => {
    latest.current = { nodes, focus, width, height, inset }
  })

  const fit = useCallback(() => {
    const { nodes, focus, width, height, inset } = latest.current
    const target = fitTarget(viewNodes(nodes), width, height, inset, focus)
    void fitView({
      nodes: target.map(({ id }) => ({ id })),
      padding: { top: `${inset.top}px`, right: `${inset.right}px`, bottom: `${inset.bottom}px`, left: `${inset.left}px` },
      minZoom: READABLE_ZOOM,
      maxZoom: READABLE_ZOOM,
      duration: FIT_DURATION,
    })
  }, [fitView])

  useEffect(() => {
    // Fit after mount and on every layout-direction change. A short settle
    // delay lets React Flow register node measurements + container size; an
    // immediate fitView no-ops (the design-system graph demo hit the same
    // timing quirk). `fitView` is safe to call repeatedly.
    const t = setTimeout(fit, 150)
    return () => clearTimeout(t)
  }, [layoutKey, fit])

  useImperativeHandle(handleRef, () => ({ fit }), [fit])
  return null
}

/** The node a key event came from, when it came from one. */
function nodeIdOf(event: KeyboardEvent): string | null {
  return event.target instanceof Element ? event.target.closest('.react-flow__node')?.getAttribute('data-id') ?? null : null
}

export function GraphCanvas({
  assets,
  dir,
  selected,
  onSelect,
  onOpenGroup,
  states,
  handleRef,
}: GraphCanvasProps) {
  // Re-layout only when structure or direction changes — never on selection or
  // a live status tick. A big graph is laid out in a worker: `null` until it is.
  const laidOut = useGraphLayout(assets, dir)
  const base = laidOut ?? NO_GRAPH

  const nodes = useMemo<GraphNode[]>(() => {
    const names = new Map(base.nodes.map((n) => [n.id, n.data.name]))
    // A consumer that did not run says which failure upstream kept it from running. A node
    // whose state has not arrived is not one that did not run.
    const statuses = new Map<string, StatusKind>()
    for (const n of base.nodes) {
      const state = states[n.id]
      if (state && state !== PENDING_GRAPH_STATE) statuses.set(n.id, state.status)
    }
    const blocked = blockedLabels(base.edges.map((e) => [e.source, e.target]), statuses, (id) => names.get(id) ?? id)
    return base.nodes.map((n) => {
      const state = states[n.id] ?? UNKNOWN_GRAPH_STATE
      const why = blocked.get(n.id)
      return { ...n, selected: n.id === selected,
        data: { ...n.data, status: state.status, metric: why?.label ?? state.label, stateHint: why ? `${why.hint} ${state.hint}` : state.hint } }
    })
  }, [base.nodes, base.edges, selected, states])

  const nodeStates = useMemo(() => new Map(nodes.map(n => [n.id, n.data.status])), [nodes])
  const edges = useMemo(
    () =>
      base.edges.map((e) => {
        const hot = e.source === selected || e.target === selected
        const { weight, port, loose } = e.data as GraphEdgeData
        return {
          ...e,
          hidden: loose && !hot,
          // A count on each of a dense map's edges would be a wall of numbers.
          label: weight && (hot || base.edges.length <= LABEL_EDGES_UP_TO) ? weight.toLocaleString() : undefined,
          className: [
            edgeClassName({ sourceStatus: nodeStates.get(e.source), targetStatus: nodeStates.get(e.target), hot }),
            port ? 'port' : '',
            weight ? 'weighted' : '',
          ].filter(Boolean).join(' '),
        }
      }),
    [base.edges, selected, nodeStates],
  )

  const onNodeClick = useCallback((_: unknown, n: Node) => {
    if (n.type !== 'port') onSelect(n.id)
  }, [onSelect])

  // Arrow keys walk the graph from the focused (else selected) node; Enter opens what has
  // focus: a group when the page opens groups, a node's panel otherwise; Escape closes it.
  // Focusing a node is what React Flow pans to when it is off screen.
  const onKeyDown = useCallback((event: KeyboardEvent) => {
    if (event.target instanceof HTMLElement && /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) return
    const id = nodeIdOf(event)
    if (isArrowKey(event.key)) {
      const next = neighborInDirection(viewNodes(nodes), id ?? selected, event.key)
      event.preventDefault()
      if (!next) return
      onSelect(next)
      document.querySelector<HTMLElement>(`.react-flow__node[data-id="${CSS.escape(next)}"]`)?.focus()
    } else if (event.key === 'Enter' && id) {
      const node = nodes.find((n) => n.id === id)
      if (!node || node.type === 'port') return
      event.preventDefault()
      if (node.data.kind === 'group' && onOpenGroup) onOpenGroup(id)
      else onSelect(id)
    } else if (event.key === 'Escape' && selected) {
      onSelect(null)
    }
  }, [nodes, selected, onSelect, onOpenGroup])

  const minimap = base.nodes.length >= MINIMAP_FROM && base.nodes.length <= MINIMAP_UP_TO

  if (!laidOut) {
    return (
      <div className="barca-graph-pending" role="status">
        Laying out {assets.length} nodes…
      </div>
    )
  }

  return (
    <ReactFlowProvider>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        onNodeClick={onNodeClick}
        onNodeDoubleClick={(_, node) => node.data.kind === 'group' && onOpenGroup?.(node.id)}
        onKeyDown={onKeyDown}
        // A click on the pane focuses the canvas, so the arrow keys work from there too.
        tabIndex={-1}
        zoomOnDoubleClick={!onOpenGroup}
        onPaneClick={() => onSelect(null)}
        minZoom={0.05}
        maxZoom={1.6}
        nodesDraggable={false}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
      >
        <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="var(--graph-grid)" />
        <Controls showInteractive={false} />
        {minimap && <MiniMap nodeColor={n => statusMeta(nodeStates.get(n.id) ?? 'skipped').color} pannable zoomable maskColor="var(--graph-mask)" nodeStrokeWidth={0} />}
        <FitController nodes={nodes} layoutKey={`${dir}:${base.nodes.map(n => n.id).join("|")}`} focus={selected && domainFile(selected) ? null : selected} minimap={minimap} handleRef={handleRef} />
      </ReactFlow>
    </ReactFlowProvider>
  )
}
