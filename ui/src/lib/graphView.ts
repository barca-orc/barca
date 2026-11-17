/**
 * What the graph view does with the space it has, apart from React Flow: which nodes a fit
 * shows and where the keyboard goes next. Layout only: which node is blocked, and by which
 * failing node, is the server's answer on each row (`root`, read by `blockerOf`).
 */


/** A node as the view sees it: its box in flow coordinates, and whether it needs attention. */
export interface ViewNode {
  id: string
  x: number
  y: number
  width: number
  height: number
  /** Its last attempt failed: what a fit centres on and the keyboard starts from. */
  failed: boolean
  /** A run is working on it: what a fit centres on when nothing failed. */
  live?: boolean
  /** Not selectable (a port); the keyboard skips it. */
  inert?: boolean
}

/** Room around a fit, in screen pixels. */
export interface Inset {
  top: number
  right: number
  bottom: number
  left: number
}

/** A React Flow viewport: the screen position of the flow origin, and the zoom. */
export interface Viewport {
  x: number
  y: number
  zoom: number
}

/** The zoom at which a card is 1:1 with its design (13px names): a fit never zooms in past it. */
export const READABLE_ZOOM = 1

/** The smallest text a card draws (`--text-xs`: a kind, a key count, a port's name), in px. */
export const SMALLEST_LABEL_PX = 12
/** The least any label may render at on screen, in px. */
export const MIN_LABEL_PX = 11

/**
 * The least a fit zooms out to: the smallest label on a card still renders at 11px. A graph that
 * does not fit at this zoom is shown at it, around its most relevant part, and the rest is a pan
 * away.
 */
export const MIN_FIT_ZOOM = MIN_LABEL_PX / SMALLEST_LABEL_PX

/** Of the height a fitted graph leaves free, the share above it (the rest is below), at most so many px. */
const OPTICAL_TOP = 0.35
const OPTICAL_TOP_MAX = 120

function bounds(nodes: readonly ViewNode[]) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity
  for (const n of nodes) {
    x0 = Math.min(x0, n.x)
    y0 = Math.min(y0, n.y)
    x1 = Math.max(x1, n.x + n.width)
    y1 = Math.max(y1, n.y + n.height)
  }
  return { x0, y0, x1, y1, width: x1 - x0, height: y1 - y0 }
}

/** Whether `nodes` fit in a `width` × `height` screen at the least fit zoom, inside `inset`. */
export function fitsReadably(nodes: readonly ViewNode[], width: number, height: number, inset: Inset): boolean {
  if (nodes.length === 0) return true
  const b = bounds(nodes)
  return b.width * MIN_FIT_ZOOM <= width - inset.left - inset.right && b.height * MIN_FIT_ZOOM <= height - inset.top - inset.bottom
}

/** Upstream first: the reading order of a left-to-right or top-down layout. */
function byPosition(a: ViewNode, b: ViewNode): number {
  return a.x - b.x || a.y - b.y
}

/** `nodes` if they fit together at the least fit zoom, else the first of them in reading order. */
function together(nodes: ViewNode[], width: number, height: number, inset: Inset): ViewNode[] {
  const sorted = [...nodes].sort(byPosition)
  return fitsReadably(sorted, width, height, inset) ? sorted : sorted.slice(0, 1)
}

/**
 * The nodes a fit shows: the whole graph when it fits at a readable zoom; else the selected
 * node; else the failed nodes (together when they fit, else the first); else the nodes a run is
 * working on; else the start of the graph (its upstream-most node). An unmeasured screen (no
 * size yet) fits everything, as before measurement.
 */
export function fitTarget(nodes: ViewNode[], width: number, height: number, inset: Inset, selected: string | null): ViewNode[] {
  if (width <= 0 || height <= 0 || fitsReadably(nodes, width, height, inset)) return nodes
  const picked = nodes.find((n) => n.id === selected)
  if (picked) return [picked]
  const failed = nodes.filter((n) => n.failed)
  if (failed.length > 0) return together(failed, width, height, inset)
  const live = nodes.filter((n) => n.live)
  if (live.length > 0) return together(live, width, height, inset)
  const start = nodes.filter((n) => !n.inert).sort(byPosition)[0] ?? nodes[0]
  return start ? [start] : nodes
}

/**
 * The viewport a fit sets, never below `MIN_FIT_ZOOM`: a graph that fits whole is shown whole,
 * at most 1:1, centred across and a little above the middle; a bigger one is shown at the least
 * fit zoom, as much of it as fits there, centred on `fitTarget`, and moved
 * back so no more of the screen than the inset is left empty past the graph's edges. `whole`
 * shows everything, at any zoom: the user asked for the overview.
 */
export function fitViewport(nodes: ViewNode[], width: number, height: number, inset: Inset, selected: string | null, whole = false): Viewport | null {
  if (nodes.length === 0 || width <= 0 || height <= 0) return null
  const all = bounds(nodes)
  const roomX = Math.max(1, width - inset.left - inset.right)
  const roomY = Math.max(1, height - inset.top - inset.bottom)
  const target = whole ? nodes : fitTarget(nodes, width, height, inset, selected)
  if (target === nodes) {
    const zoom = Math.max(0.05, Math.min(READABLE_ZOOM, roomX / all.width, roomY / all.height))
    const free = Math.max(0, roomY - all.height * zoom)
    return {
      x: inset.left + (roomX - all.width * zoom) / 2 - all.x0 * zoom,
      y: inset.top + Math.min(free * OPTICAL_TOP, OPTICAL_TOP_MAX) - all.y0 * zoom,
      zoom,
    }
  }
  // As much of the graph as fits at the least fit zoom, around the target (which fits at it).
  const t = bounds(target)
  const zoom = MIN_FIT_ZOOM
  const axis = (lo: number, hi: number, t0: number, t1: number, room: number, before: number) => {
    // The screen offset that centres the target, then held so the graph's edge is no further in than the inset.
    const centred = before + room / 2 - ((t0 + t1) / 2) * zoom
    if ((hi - lo) * zoom <= room) return before + (room - (hi - lo) * zoom) / 2 - lo * zoom
    const most = before - lo * zoom
    const least = before + room - hi * zoom
    return Math.min(most, Math.max(least, centred))
  }
  return {
    x: axis(all.x0, all.x1, t.x0, t.x1, roomX, inset.left),
    y: axis(all.y0, all.y1, t.y0, t.y1, roomY, inset.top),
    zoom,
  }
}

/**
 * The viewport that brings `node` into the screen with room around it, keeping the zoom and
 * moving as little as it can; null when it is already in view. What the graph does when a side
 * panel opens over (or narrows) the canvas, or a selection lands off screen.
 */
export function revealViewport(viewport: Viewport, node: Pick<ViewNode, 'x' | 'y' | 'width' | 'height'>, width: number, height: number, inset: Inset): Viewport | null {
  if (width <= 0 || height <= 0) return null
  const { zoom } = viewport
  const shift = (start: number, size: number, before: number, after: number, screen: number, offset: number) => {
    const lo = start * zoom + offset
    const hi = (start + size) * zoom + offset
    const room = screen - before - after
    if (size * zoom > room) return before - lo + (room - size * zoom) / 2
    if (lo < before) return before - lo
    if (hi > screen - after) return screen - after - hi
    return 0
  }
  const dx = shift(node.x, node.width, inset.left, inset.right, width, viewport.x)
  const dy = shift(node.y, node.height, inset.top, inset.bottom, height, viewport.y)
  if (Math.abs(dx) < 0.5 && Math.abs(dy) < 0.5) return null
  return { x: viewport.x + dx, y: viewport.y + dy, zoom }
}

type ArrowKey = 'ArrowLeft' | 'ArrowRight' | 'ArrowUp' | 'ArrowDown'

export function isArrowKey(key: string): key is ArrowKey {
  return key === 'ArrowLeft' || key === 'ArrowRight' || key === 'ArrowUp' || key === 'ArrowDown'
}

/**
 * The node an arrow key moves to from `from`: the nearest one in that direction, preferring
 * those roughly in line (a sideways step counts double). With nothing to start from, the first
 * failed node, else the upstream-most one.
 */
export function neighborInDirection(nodes: ViewNode[], from: string | null, key: ArrowKey): string | null {
  const live = nodes.filter((n) => !n.inert)
  const origin = live.find((n) => n.id === from)
  if (!origin) {
    const start = live.filter((n) => n.failed).sort(byPosition)[0] ?? [...live].sort(byPosition)[0]
    return start?.id ?? null
  }
  const center = (n: ViewNode) => ({ x: n.x + n.width / 2, y: n.y + n.height / 2 })
  const o = center(origin)
  let best: { id: string; cost: number } | null = null
  for (const n of live) {
    if (n.id === origin.id) continue
    const c = center(n)
    const dx = c.x - o.x
    const dy = c.y - o.y
    const ahead = key === 'ArrowRight' ? dx : key === 'ArrowLeft' ? -dx : key === 'ArrowDown' ? dy : -dy
    if (ahead <= 0) continue
    const aside = Math.abs(key === 'ArrowRight' || key === 'ArrowLeft' ? dy : dx)
    const cost = ahead + 2 * aside
    if (!best || cost < best.cost) best = { id: n.id, cost }
  }
  return best?.id ?? null
}
