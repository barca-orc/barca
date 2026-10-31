import type { StatusKind } from '@/lib/types'

/**
 * What the graph view does with the space it has, apart from React Flow: which nodes a fit
 * shows, where the keyboard goes next, and why a consumer did not run.
 */

/** A node as the view sees it: its box in flow coordinates, and its state. */
export interface ViewNode {
  id: string
  x: number
  y: number
  width: number
  height: number
  status: StatusKind
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

/**
 * The zoom at which a card is 1:1 with its design: 13px names, 12px labels. A fit never goes
 * below it; a graph that does not fit at this zoom is shown from its selected or failed nodes.
 */
export const READABLE_ZOOM = 1

function bounds(nodes: ViewNode[]) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity
  for (const n of nodes) {
    x0 = Math.min(x0, n.x)
    y0 = Math.min(y0, n.y)
    x1 = Math.max(x1, n.x + n.width)
    y1 = Math.max(y1, n.y + n.height)
  }
  return { width: x1 - x0, height: y1 - y0 }
}

/** Whether `nodes` fit in a `width` × `height` screen at the readable zoom, inside `inset`. */
export function fitsReadably(nodes: ViewNode[], width: number, height: number, inset: Inset): boolean {
  if (nodes.length === 0) return true
  const b = bounds(nodes)
  return b.width * READABLE_ZOOM <= width - inset.left - inset.right && b.height * READABLE_ZOOM <= height - inset.top - inset.bottom
}

/** Upstream first: the reading order of a left-to-right or top-down layout. */
function byPosition(a: ViewNode, b: ViewNode): number {
  return a.x - b.x || a.y - b.y
}

/**
 * The nodes a fit shows: the whole graph when it fits at the readable zoom; else the selected
 * node; else the failed nodes, or the first of them when they are too far apart; else the whole
 * graph, centred. An unmeasured screen (no size yet) fits everything, as before measurement.
 */
export function fitTarget(nodes: ViewNode[], width: number, height: number, inset: Inset, selected: string | null): ViewNode[] {
  if (width <= 0 || height <= 0 || fitsReadably(nodes, width, height, inset)) return nodes
  const picked = nodes.find((n) => n.id === selected)
  if (picked) return [picked]
  const failed = nodes.filter((n) => n.status === 'failed').sort(byPosition)
  if (failed.length === 0) return nodes
  return fitsReadably(failed, width, height, inset) ? failed : failed.slice(0, 1)
}

export type ArrowKey = 'ArrowLeft' | 'ArrowRight' | 'ArrowUp' | 'ArrowDown'

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
    const start = live.filter((n) => n.status === 'failed').sort(byPosition)[0] ?? [...live].sort(byPosition)[0]
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

/** A consumer that did not run, and the failed node upstream that explains it. */
export interface Blocked { label: string; hint: string }

/**
 * Why each skipped node did not run, when a failed node upstream says so: a direct input that
 * failed, or the failure behind an input that is blocked itself. Nodes in other states (stale,
 * healthy, running) keep their own labels; the server knows those better.
 */
export function blockedLabels(
  edges: [source: string, target: string][],
  status: Map<string, StatusKind>,
  name: (id: string) => string,
): Map<string, Blocked> {
  const inputs = new Map<string, string[]>()
  for (const [source, target] of edges) inputs.set(target, [...(inputs.get(target) ?? []), source])
  const cause = new Map<string, string | null>()
  const causeOf = (id: string, seen: Set<string>): string | null => {
    if (cause.has(id)) return cause.get(id)!
    if (seen.has(id)) return null
    seen.add(id)
    let found: string | null = null
    for (const input of inputs.get(id) ?? []) {
      const s = status.get(input)
      if (s === 'failed') found = input
      else if (s === 'skipped') found = causeOf(input, seen)
      if (found) break
    }
    cause.set(id, found)
    return found
  }
  const out = new Map<string, Blocked>()
  for (const [id, s] of status) {
    if (s !== 'skipped') continue
    const failed = causeOf(id, new Set())
    if (failed) out.set(id, { label: `Blocked: ${name(failed)} failed`, hint: `Not run: its input ${name(failed)} failed.` })
  }
  return out
}
