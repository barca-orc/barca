import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type Ref } from 'react'
import { plural } from '@/lib/plural'
import { ArrowDown, ArrowRight, Maximize, Minus, Plus } from 'lucide-react'
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  BackgroundVariant,
  ControlButton,
  Controls,
  MiniMap,
  Panel,
  useReactFlow,
  useStore,
  type Edge,
  type Node,
  type NodeTypes,
} from '@xyflow/react'
import { AssetNode } from './AssetNode'
import { GroupNode } from './GroupNode'
import { PortNode } from './PortNode'
import { GraphLegend } from './GraphLegend'
import { BLOCKED_UPSTREAM, PINNED } from '@/lib/cacheStateDisplay'
import { fitTarget, isArrowKey, MIN_FIT_ZOOM, neighborInDirection, READABLE_ZOOM, waitStatus, waitsOn, type Inset, type ViewNode, type WaitStatus } from '@/lib/graphView'
import { isLive, UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { isFailed } from '@/lib/health'
import { edgeClassName, type LayoutDir, type GraphEdgeData, type GraphNode, type GraphEntry } from '@/lib/graph'
import { domainFile } from '@/lib/domainMap'
import { useGraphLayout } from '@/hooks/useGraphLayout'
import { glyphColor, toneGlyph } from '@/lib/glyph'

// Defined once, outside the component — a fresh object each render is a perf bug.
const nodeTypes: NodeTypes = { asset: AssetNode, nodeGroup: GroupNode, port: PortNode }

/**
 * Room around a fit: 48px on every side, and above the bottom row of what floats over the
 * canvas (the legend at the left, the zoom controls at the right); the minimap's width at the
 * right when there is one.
 */
const FIT_INSET: Inset = { top: 48, right: 48, bottom: 72, left: 48 }
const FIT_INSET_BESIDE_MINIMAP: Inset = { ...FIT_INSET, right: 236 }
const FIT_DURATION = 200
/** Of the height a fitted graph leaves free, the share above it (the rest is below), at most so many px. */
const OPTICAL_TOP = 0.35
const OPTICAL_TOP_MAX = 120

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
  /** Pinned nodes (`GET /pins`) and what each is pinned to: they read `pinned`. */
  pins?: ReadonlyMap<string, string>
  /** Legend entries of the page's own (a group card), before the states. */
  legend?: ReactNode
  /** The nodes a filter keeps; the others (and their edges) are dimmed. null: no filter. */
  shown?: ReadonlySet<string> | null
  /** Flip the layout direction: a control beside the zoom when given. */
  onToggleDir?: () => void
  handleRef?: Ref<GraphCanvasHandle>
}

const NO_PINS: ReadonlyMap<string, string> = new Map()

function viewNodes(nodes: GraphNode[]): ViewNode[] {
  return nodes.map((n) => ({
    id: n.id,
    x: n.position.x,
    y: n.position.y,
    width: n.measured?.width ?? n.initialWidth ?? 0,
    height: n.measured?.height ?? n.initialHeight ?? 0,
    failed: n.data.lastFailed ?? false,
    inert: n.type === 'port',
  }))
}

/**
 * Fit the view once custom nodes are measured, and re-fit whenever the layout
 * direction flips. React Flow can't fit unmeasured nodes, so the `fitView` prop
 * alone races the first paint of DOM-rendered custom nodes.
 *
 * A fit shows a graph whole, centred with room on every side, between 0.8 and 1:1; a graph too
 * big for that is shown at 0.8 from its selected node, else its failed nodes (`fitTarget`).
 */
function FitController({ nodes, layoutKey, focus, minimap, handleRef }: { nodes: GraphNode[]; layoutKey: string; focus: string | null; minimap: boolean; handleRef?: Ref<GraphCanvasHandle> }) {
  const { fitView, setViewport } = useReactFlow()
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
    const all = viewNodes(nodes)
    const target = fitTarget(all, width, height, inset, focus)
    if (target === all && all.length > 0 && width > 0 && height > 0) {
      // The whole graph fits: at most 1:1, centred across, and placed a little above the middle
      // (optical centre), so a small graph does not float in the middle of a tall canvas.
      const x0 = Math.min(...all.map((n) => n.x))
      const y0 = Math.min(...all.map((n) => n.y))
      const w = Math.max(...all.map((n) => n.x + n.width)) - x0
      const h = Math.max(...all.map((n) => n.y + n.height)) - y0
      const roomX = width - inset.left - inset.right
      const roomY = height - inset.top - inset.bottom
      const zoom = Math.max(MIN_FIT_ZOOM, Math.min(READABLE_ZOOM, roomX / w, roomY / h))
      const free = Math.max(0, roomY - h * zoom)
      void setViewport({
        x: inset.left + (roomX - w * zoom) / 2 - x0 * zoom,
        y: inset.top + Math.min(free * OPTICAL_TOP, OPTICAL_TOP_MAX) - y0 * zoom,
        zoom,
      }, { duration: FIT_DURATION })
      return
    }
    void fitView({
      nodes: target.map(({ id }) => ({ id })),
      padding: { top: `${inset.top}px`, right: `${inset.right}px`, bottom: `${inset.bottom}px`, left: `${inset.left}px` },
      minZoom: MIN_FIT_ZOOM,
      maxZoom: READABLE_ZOOM,
      duration: FIT_DURATION,
    })
  }, [fitView, setViewport])

  // Fit once the nodes are measured and the canvas has a size, and again on a new layout. A fit
  // on a timer alone no-ops when the nodes are measured late (a slow status load), leaving the
  // graph at the canvas's top-left corner.
  // Every node has a measured size (React Flow's useNodesInitialized also waits for handle
  // bounds, which a node without edges never gets).
  const measured = useStore((s) => {
    if (s.nodeLookup.size === 0) return false
    for (const n of s.nodeLookup.values()) if (!n.hidden && !n.measured.width) return false
    return true
  })
  const sized = width > 0 && height > 0
  // Until the user pans or zooms, a canvas that changes size (a banner above it, the panel
  // opening, the window) re-fits; after that the view is theirs until the layout changes.
  const moved = useRef(false)
  const pane = useStore((s) => s.domNode)
  useEffect(() => {
    if (!pane) return
    const mine = () => { moved.current = true }
    pane.addEventListener('wheel', mine, { passive: true })
    pane.addEventListener('pointerdown', mine)
    return () => {
      pane.removeEventListener('wheel', mine)
      pane.removeEventListener('pointerdown', mine)
    }
  }, [pane])
  useEffect(() => {
    moved.current = false
  }, [layoutKey])
  useEffect(() => {
    if (!measured || !sized || moved.current) return
    // A short settle so React Flow has registered every measurement; `fit` is safe to repeat.
    const t = setTimeout(fit, 60)
    return () => clearTimeout(t)
  }, [layoutKey, fit, measured, sized, width, height])

  useImperativeHandle(handleRef, () => ({ fit }), [fit])
  return null
}

/**
 * The canvas's controls, one quiet row at the bottom right: zoom, fit (the same fit as on
 * load) and, when the page can, the layout direction. Lucide icons, like every other control.
 */
function GraphControls({ dir, onFit, onToggleDir }: { dir: LayoutDir; onFit: () => void; onToggleDir?: () => void }) {
  const { zoomIn, zoomOut } = useReactFlow()
  const dirLabel = dir === 'LR' ? 'Top-down layout' : 'Left-right layout'
  return (
    <Controls showZoom={false} showFitView={false} showInteractive={false} position="bottom-right" orientation="horizontal">
      <ControlButton aria-label="Zoom in" title="Zoom in" onClick={() => void zoomIn({ duration: FIT_DURATION })}><Plus size={14} /></ControlButton>
      <ControlButton aria-label="Zoom out" title="Zoom out" onClick={() => void zoomOut({ duration: FIT_DURATION })}><Minus size={14} /></ControlButton>
      <ControlButton aria-label="Fit to screen" title="Fit to screen" onClick={onFit}><Maximize size={13} /></ControlButton>
      {onToggleDir && (
        <ControlButton aria-label={dirLabel} title={dirLabel} onClick={onToggleDir}>
          {dir === 'LR' ? <ArrowDown size={14} /> : <ArrowRight size={14} />}
        </ControlButton>
      )}
    </Controls>
  )
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
  pins = NO_PINS,
  legend,
  shown = null,
  onToggleDir,
  handleRef,
}: GraphCanvasProps) {
  // The canvas's own Fit control and the page's (`handleRef`) are the same fit.
  const fitRef = useRef<GraphCanvasHandle>(null)
  useImperativeHandle(handleRef, () => ({ fit: () => fitRef.current?.fit() }), [])
  // The node under the pointer: its edges light up like the selected node's.
  const [hovered, setHovered] = useState<string | null>(null)
  // Re-layout only when structure or direction changes — never on selection or
  // a live status tick. A big graph is laid out in a worker: `null` until it is.
  const laidOut = useGraphLayout(assets, dir)
  const base = laidOut ?? NO_GRAPH

  const nodes = useMemo<GraphNode[]>(() => {
    const names = new Map(base.nodes.map((n) => [n.id, n.data.name]))
    // A blocked consumer (never run or stale behind a failure) reads `blocked`; its hint names the failure upstream that kept it from running. A node
    // whose state has not arrived is not one that never ran.
    const waits = new Map<string, WaitStatus>()
    for (const n of base.nodes) {
      const state = states[n.id]
      if (!state || state.kind !== 'cache') continue
      const s = waitStatus(state)
      // A pinned node is read as its pinned version: it never waits on a failure above it.
      waits.set(n.id, s === 'waiting' && pins.has(n.id) ? 'other' : s)
    }
    const why = waitsOn(base.edges.map((e) => [e.source, e.target]), waits, (id) => names.get(id) ?? id)
    return base.nodes.map((n) => {
      const state = states[n.id] ?? UNKNOWN_GRAPH_STATE
      const wait = why.get(n.id)
      // One state per node: failing > running > pinned > blocked > the cache state.
      const pin = !isFailed(state) && !isLive(state) && state.kind === 'cache' ? pins.get(n.id) : undefined
      return { ...n, selected: n.id === selected,
        className: shown && n.type !== 'port' && !shown.has(n.id) ? 'is-dimmed' : undefined,
        data: {
          ...n.data,
          tone: pin !== undefined || wait ? 'warning' : state.tone,
          glyph: pin !== undefined ? 'pinned' : wait ? 'blocked' : state.glyph,
          label: pin !== undefined ? PINNED : wait ? BLOCKED_UPSTREAM : state.label,
          blockedBy: pin === undefined && wait ? names.get(wait.by) ?? wait.by : undefined,
          pinned: pin,
          lastFailed: isFailed(state),
          live: isLive(state),
          stateHint: pin !== undefined ? `Pinned: ${pin}. Runs read this version until it is unpinned.` : wait ? `${state.hint} ${wait.hint}` : state.hint,
          keys: state.kind === 'cache' ? state.keys : undefined,
        } }
    })
  }, [base.nodes, base.edges, selected, states, pins, shown])

  const nodeTones = useMemo(() => new Map(nodes.map(n => [n.id, n.data.tone])), [nodes])
  const nodeGlyphs = useMemo(() => new Map(nodes.map(n => [n.id, n.data.glyph ?? toneGlyph(n.data.tone)])), [nodes])
  const edges = useMemo(
    () =>
      base.edges.map((e) => {
        const hot = e.source === selected || e.target === selected || (hovered !== null && (e.source === hovered || e.target === hovered))
        const { weight, port, loose } = e.data as GraphEdgeData
        return {
          ...e,
          hidden: loose && !hot,
          // A count on each of a dense map's edges would be a wall of numbers.
          label: weight && (hot || base.edges.length <= LABEL_EDGES_UP_TO) ? weight.toLocaleString() : undefined,
          className: [
            edgeClassName({ sourceTone: nodeTones.get(e.source), targetTone: nodeTones.get(e.target), hot }),
            port ? 'port' : '',
            weight ? 'weighted' : '',
            shown && !(shown.has(e.source) && shown.has(e.target)) ? 'is-dimmed' : '',
          ].filter(Boolean).join(' '),
        }
      }),
    [base.edges, selected, hovered, nodeTones, shown],
  )
  // The legend names only the states this graph shows.
  const shownGlyphs = useMemo(() => {
    const words = new Map<string, string>()
    for (const n of nodes) {
      if (n.type === 'port') continue
      const glyph = n.data.lastFailed ? 'failed' : n.data.glyph ?? toneGlyph(n.data.tone)
      if (!words.has(glyph) && n.data.label) words.set(glyph, n.data.label)
    }
    return words
  }, [nodes])

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
        Laying out {plural(assets.length, 'node')}…
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
        onNodeMouseEnter={(_, n) => setHovered(n.id)}
        onNodeMouseLeave={() => setHovered(null)}
        minZoom={0.05}
        maxZoom={1.6}
        nodesDraggable={false}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
      >
        <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="var(--graph-grid)" />
        <GraphControls dir={dir} onFit={() => fitRef.current?.fit()} onToggleDir={onToggleDir} />
        <Panel position="bottom-left">
          <GraphLegend shown={shownGlyphs}>{legend}</GraphLegend>
        </Panel>
        {minimap && <MiniMap position="top-right" nodeColor={n => glyphColor(nodeGlyphs.get(n.id) ?? 'pending')} pannable zoomable maskColor="var(--graph-mask)" nodeStrokeWidth={0} />}
        <FitController nodes={nodes} layoutKey={`${dir}:${base.nodes.map(n => n.id).join("|")}`} focus={selected && domainFile(selected) ? null : selected} minimap={minimap} handleRef={fitRef} />
      </ReactFlow>
    </ReactFlowProvider>
  )
}
