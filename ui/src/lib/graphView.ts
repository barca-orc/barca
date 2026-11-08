/**
 * What the graph view does with the space it has, apart from React Flow: which nodes a fit
 * shows, where the keyboard goes next, and what a never-run consumer waits on.
 */

import { isFailed } from './health'
import { memberHealth, type GraphState } from './graphState'
import type { NodeState } from './types'

/** A node as the view sees it: its box in flow coordinates, and whether it needs attention. */
export interface ViewNode {
  id: string
  x: number
  y: number
  width: number
  height: number
  /** Its last attempt failed: what a fit centres on and the keyboard starts from. */
  failed: boolean
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

/** The zoom at which a card is 1:1 with its design (13px names): a fit never zooms in past it. */
export const READABLE_ZOOM = 1

/**
 * The least a fit zooms out to show a whole graph: names stay above 10px. A graph that does not
 * fit even at this zoom is shown at it from its selected or failed nodes.
 */
export const MIN_FIT_ZOOM = 0.8

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

/** Whether `nodes` fit in a `width` × `height` screen at the least fit zoom, inside `inset`. */
export function fitsReadably(nodes: ViewNode[], width: number, height: number, inset: Inset): boolean {
  if (nodes.length === 0) return true
  const b = bounds(nodes)
  return b.width * MIN_FIT_ZOOM <= width - inset.left - inset.right && b.height * MIN_FIT_ZOOM <= height - inset.top - inset.bottom
}

/** Upstream first: the reading order of a left-to-right or top-down layout. */
function byPosition(a: ViewNode, b: ViewNode): number {
  return a.x - b.x || a.y - b.y
}

/**
 * The nodes a fit shows: the whole graph when it fits at a readable zoom; else the selected
 * node; else the failed nodes, or the first of them when they are too far apart; else the whole
 * graph, centred. An unmeasured screen (no size yet) fits everything, as before measurement.
 */
export function fitTarget(nodes: ViewNode[], width: number, height: number, inset: Inset, selected: string | null): ViewNode[] {
  if (width <= 0 || height <= 0 || fitsReadably(nodes, width, height, inset)) return nodes
  const picked = nodes.find((n) => n.id === selected)
  if (picked) return [picked]
  const failed = nodes.filter((n) => n.failed).sort(byPosition)
  if (failed.length === 0) return nodes
  return fitsReadably(failed, width, height, inset) ? failed : failed.slice(0, 1)
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

/** What a node waits on: the failed node upstream that kept it from running, and that in a line. */
export interface WaitsOn { by: string; hint: string }

/**
 * How a node stands in the wait: its last attempt failed, it is waiting to be brought up to
 * date (never run or stale), or neither.
 */
export type WaitStatus = 'failed' | 'waiting' | 'other'

/** A node's wait status from its box: running and unloaded nodes are neither failed nor waiting. */
export function waitStatus(state: GraphState): WaitStatus {
  if (isFailed(state)) return 'failed'
  return state.kind === 'cache' && (state.state === 'never_run' || state.state === 'stale') ? 'waiting' : 'other'
}

/**
 * For each waiting node, the failed node upstream that explains it: a direct input whose last
 * attempt failed, or the failure behind an input that is waiting itself. Such a node is
 * `blocked`: it cannot be brought up to date until that failure is fixed.
 */
export function waitsOn(
  edges: [source: string, target: string][],
  status: ReadonlyMap<string, WaitStatus>,
  name: (id: string) => string,
): Map<string, WaitsOn> {
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
      else if (s === 'waiting') found = causeOf(input, seen)
      if (found) break
    }
    cause.set(id, found)
    return found
  }
  const out = new Map<string, WaitsOn>()
  for (const [id, s] of status) {
    if (s !== 'waiting') continue
    const failed = causeOf(id, new Set())
    if (failed) out.set(id, { by: failed, hint: `Blocked: waits on ${name(failed)}, whose last attempt failed.` })
  }
  return out
}

/**
 * Each blocked node of `nodes` and the failed node upstream it waits on (by id). A pinned node
 * (`pinned`) is never waiting: runs read its pinned version, so it does not pass a failure
 * above it on to what reads it.
 */
export function blockedBy(
  nodes: readonly NodeState[],
  graph: readonly { id: string; inputs: readonly string[] }[],
  pinned: { has(id: string): boolean } = new Set<string>(),
): Map<string, string> {
  const status = new Map(nodes.map((n): [string, WaitStatus] => {
    const s = waitStatus(memberHealth(n))
    return [n.id, s === 'waiting' && pinned.has(n.id) ? 'other' : s]
  }))
  const edges = graph.flatMap((g): [string, string][] => g.inputs.map((i) => [i, g.id]))
  return new Map([...waitsOn(edges, status, (id) => id)].map(([id, w]) => [id, w.by]))
}
