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
import { buildGraph, edgeClassName, type LayoutDir, type GraphNode } from '@/lib/graph'
import type { AssetSummary, StatusKind } from '@/lib/types'

// Defined once, outside the component — a fresh object each render is a perf bug.
const nodeTypes: NodeTypes = { asset: AssetNode }

const FIT_OPTIONS = { padding: 0.18, maxZoom: 1.4, duration: 200 }

export interface GraphCanvasHandle {
  fit: () => void
}

interface GraphCanvasProps {
  assets: AssetSummary[]
  dir: LayoutDir
  selected: string | null
  onSelect: (id: string | null) => void
  /** Live per-node status overlay (from the run event stream). */
  statuses?: Record<string, StatusKind>
  handleRef?: Ref<GraphCanvasHandle>
}

/**
 * Fit the view once custom nodes are measured, and re-fit whenever the layout
 * direction flips. React Flow can't fit unmeasured nodes, so the `fitView` prop
 * alone races the first paint of DOM-rendered custom nodes.
 */
function FitController({ dir, handleRef }: { dir: LayoutDir; handleRef?: Ref<GraphCanvasHandle> }) {
  const { fitView } = useReactFlow()

  useEffect(() => {
    // Fit after mount and on every layout-direction change. A short settle
    // delay lets React Flow register node measurements + container size; an
    // immediate fitView no-ops (the design-system graph demo hit the same
    // timing quirk). `fitView` is safe to call repeatedly.
    const t = setTimeout(() => void fitView(FIT_OPTIONS), 150)
    return () => clearTimeout(t)
  }, [dir, fitView])

  useImperativeHandle(handleRef, () => ({ fit: () => void fitView(FIT_OPTIONS) }), [fitView])
  return null
}

export function GraphCanvas({
  assets,
  dir,
  selected,
  onSelect,
  statuses,
  handleRef,
}: GraphCanvasProps) {
  // Re-layout only when structure or direction changes — never on selection or
  // a live status tick.
  const base = useMemo(() => buildGraph(assets, dir), [assets, dir])

  const nodes = useMemo<GraphNode[]>(
    () =>
      base.nodes.map((n) => ({
        ...n,
        selected: n.id === selected,
        // Merge the live status overlay without recomputing the dagre layout.
        data: { ...n.data, status: statuses?.[n.id] ?? n.data.status },
      })),
    [base.nodes, selected, statuses],
  )

  const edges = useMemo(
    () =>
      base.edges.map((e) => ({
        ...e,
        className: edgeClassName({
          sourceStatus: statuses?.[e.source],
          targetStatus: statuses?.[e.target],
          hot: e.source === selected || e.target === selected,
        }),
      })),
    [base.edges, selected, statuses],
  )

  const onNodeClick = useCallback((_: unknown, n: Node) => onSelect(n.id), [onSelect])

  return (
    <ReactFlowProvider>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        onNodeClick={onNodeClick}
        onPaneClick={() => onSelect(null)}
        minZoom={0.3}
        maxZoom={1.6}
        nodesDraggable={false}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
      >
        <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="rgba(63,209,129,0.10)" />
        <Controls showInteractive={false} />
        <MiniMap pannable zoomable maskColor="rgba(8,11,10,0.72)" nodeStrokeWidth={0} />
        <FitController dir={dir} handleRef={handleRef} />
      </ReactFlow>
    </ReactFlowProvider>
  )
}
