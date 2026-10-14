import { useCallback, useEffect, useImperativeHandle, useMemo, type Ref } from 'react'
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  useReactFlow,
  type Node,
  type NodeTypes,
} from '@xyflow/react'
import { AssetNode } from './AssetNode'
import { GroupNode } from './GroupNode'
import { UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { buildGraph, edgeClassName, type LayoutDir, type GraphNode, type GraphEntry } from '@/lib/graph'
import { statusMeta } from '@/lib/status'

// Defined once, outside the component — a fresh object each render is a perf bug.
const nodeTypes: NodeTypes = { asset: AssetNode, nodeGroup: GroupNode }

const FIT_OPTIONS = { padding: 0.18, maxZoom: 1.4, duration: 200 }

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
function FitController({ layoutKey, handleRef }: { layoutKey: string; handleRef?: Ref<GraphCanvasHandle> }) {
  const { fitView } = useReactFlow()

  useEffect(() => {
    // Fit after mount and on every layout-direction change. A short settle
    // delay lets React Flow register node measurements + container size; an
    // immediate fitView no-ops (the design-system graph demo hit the same
    // timing quirk). `fitView` is safe to call repeatedly.
    const t = setTimeout(() => void fitView(FIT_OPTIONS), 150)
    return () => clearTimeout(t)
  }, [layoutKey, fitView])

  useImperativeHandle(handleRef, () => ({ fit: () => void fitView(FIT_OPTIONS) }), [fitView])
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
  // a live status tick.
  const base = useMemo(() => buildGraph(assets, dir), [assets, dir])

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
      base.edges.map((e) => ({
        ...e,
        className: edgeClassName({
          sourceStatus: nodeStates.get(e.source),
          targetStatus: nodeStates.get(e.target),
          hot: e.source === selected || e.target === selected,
        }),
      })),
    [base.edges, selected, nodeStates],
  )

  const onNodeClick = useCallback((_: unknown, n: Node) => {
    onSelect(n.id)
  }, [onSelect])

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
        minZoom={0.3}
        maxZoom={1.6}
        nodesDraggable={false}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
      >
        <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="var(--graph-grid)" />
        <Controls showInteractive={false} />
        <MiniMap nodeColor={n => statusMeta(nodeStates.get(n.id) ?? 'skipped').color} pannable zoomable maskColor="var(--graph-mask)" nodeStrokeWidth={0} />
        <FitController layoutKey={`${dir}:${base.nodes.map(n => n.id).join("|")}`} handleRef={handleRef} />
      </ReactFlow>
    </ReactFlowProvider>
  )
}
