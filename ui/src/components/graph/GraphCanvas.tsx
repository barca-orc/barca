import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, type Ref } from 'react'
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  useReactFlow,
  type Edge,
  type Node,
  type NodeTypes,
} from '@xyflow/react'
import { AssetNode } from './AssetNode'
import { GroupNode } from './GroupNode'
import { PortNode } from './PortNode'
import { UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { edgeClassName, type LayoutDir, type GraphEdgeData, type GraphNode, type GraphEntry } from '@/lib/graph'
import { domainFile } from '@/lib/domainMap'
import { useGraphLayout } from '@/hooks/useGraphLayout'
import { statusMeta } from '@/lib/status'

// Defined once, outside the component — a fresh object each render is a perf bug.
const nodeTypes: NodeTypes = { asset: AssetNode, nodeGroup: GroupNode, port: PortNode }

/** Room around a fit for what floats over the canvas: the legend, and the minimap beside it. */
const FIT_PADDING = { top: '32px', right: '32px', bottom: '64px', left: '56px' } as const
const FIT_BESIDE_MINIMAP = { ...FIT_PADDING, right: '236px' } as const
const FIT_OPTIONS = { padding: FIT_PADDING, maxZoom: 1.4, duration: 200 }
/** The view of a selected node: on it, never so small that names cannot be read. */
const FOCUS_VIEW = { ...FIT_OPTIONS, minZoom: 0.7, maxZoom: 1 }

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

/**
 * Fit the view once custom nodes are measured, and re-fit whenever the layout
 * direction flips. React Flow can't fit unmeasured nodes, so the `fitView` prop
 * alone races the first paint of DOM-rendered custom nodes.
 */
function FitController({ layoutKey, focus, minimap, handleRef }: { layoutKey: string; focus: string | null; minimap: boolean; handleRef?: Ref<GraphCanvasHandle> }) {
  const { fitView } = useReactFlow()
  const fit = useMemo(() => (minimap ? { ...FIT_OPTIONS, padding: FIT_BESIDE_MINIMAP } : FIT_OPTIONS), [minimap])
  // Read when the layout changes: selecting a node does not move the view.
  const focused = useRef(focus)
  useEffect(() => {
    focused.current = focus
  })

  useEffect(() => {
    // Fit after mount and on every layout-direction change. A short settle
    // delay lets React Flow register node measurements + container size; an
    // immediate fitView no-ops (the design-system graph demo hit the same
    // timing quirk). `fitView` is safe to call repeatedly.
    const t = setTimeout(() => {
      const id = focused.current
      void fitView(id ? { ...FOCUS_VIEW, nodes: [{ id }] } : fit)
    }, 150)
    return () => clearTimeout(t)
  }, [layoutKey, fitView, fit])

  useImperativeHandle(handleRef, () => ({ fit: () => void fitView(fit) }), [fitView, fit])
  return null
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

  const nodes = useMemo<GraphNode[]>(
    () =>
      base.nodes.map((n) => {
        const state = states[n.id] ?? UNKNOWN_GRAPH_STATE
        return { ...n, selected: n.id === selected,
          data: { ...n.data, status: state.status, metric: state.label, stateHint: state.hint } }
      }),
    [base.nodes, selected, states],
  )

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
        onKeyDown={(event) => {
          const id = event.target instanceof Element ? event.target.closest('.react-flow__node')?.getAttribute('data-id') : null
          if (event.key === 'Enter' && id && base.nodes.some(node => node.id === id && node.data.kind === 'group') && onOpenGroup) {
            event.preventDefault()
            onOpenGroup(id)
          }
        }}
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
        <FitController layoutKey={`${dir}:${base.nodes.map(n => n.id).join("|")}`} focus={selected && domainFile(selected) ? null : selected} minimap={minimap} handleRef={handleRef} />
      </ReactFlow>
    </ReactFlowProvider>
  )
}
