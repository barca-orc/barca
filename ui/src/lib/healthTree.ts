/**
 * The project as one health tree: root → group → … → node, every level the exact sum of its
 * children, so no two screens can disagree about a scope. The health map draws it as a
 * sunburst whose segments are sized by how much is wrong underneath and coloured by the worst
 * health inside; the causes list and the tables read their counts from the same tree.
 *
 * With groups (`group(...)`), the top level is the root groups plus the nodes in no group; a
 * project without groups is shown by source file. Pure: no React, no fetching.
 */

import { HEALTH_ORDER, blockedCauses, healthRank, healthWeight, isUnhealthy, nodeHealth, worstHealth, type Health } from './nodeHealth'
import { shortName } from './graph'
import { sourceFile } from './pipeline'
import type { NodeGroup, NodeState } from './types'

export const ROOT = '__root__'

export type HealthTreeKind = 'root' | 'group' | 'file' | 'node'

export interface HealthTreeNode {
  id: string
  label: string
  kind: HealthTreeKind
  /** Children, worst first, then the heaviest, then by name. */
  children: string[]
  /** Nodes underneath (itself, for a node). */
  leaves: number
  /** Nodes underneath by health. */
  counts: Record<Health, number>
  /** The worst health underneath. */
  health: Health
  /** How much is wrong underneath: unhealthy keys of partitioned nodes, else nodes. */
  weight: number
  /** Nodes underneath a run is working on now. */
  running: number
}

export interface HealthTree {
  nodes: Map<string, HealthTreeNode>
  parent: Map<string, string>
  /** Each node's own health, by node id. */
  healthOf: Map<string, Health>
  /** Each blocked node and the failing node upstream that blocks it. */
  blockedBy: Map<string, string>
}

export interface HealthTreeInput {
  states: readonly NodeState[]
  /** The graph's edges, as `GET /assets` reports them (node ids and their inputs). */
  graph: readonly { id: string; inputs: readonly string[] }[]
  groups: readonly NodeGroup[]
  /** Nodes whose output looks wrong (data signals). */
  suspect?: ReadonlySet<string>
  /** Scheduled nodes that missed their slot. */
  late?: ReadonlySet<string>
  pinned?: { has(id: string): boolean }
}

function zeroCounts(): Record<Health, number> {
  return Object.fromEntries(HEALTH_ORDER.map((h) => [h, 0])) as Record<Health, number>
}

/** Build the tree. Nodes without a state row are left out: their health is not known yet. */
export function buildHealthTree(input: HealthTreeInput): HealthTree {
  const byId = new Map(input.states.map((s) => [s.id, s]))
  const inputs = new Map<string, readonly string[]>()
  for (const g of input.graph) inputs.set(g.id, g.inputs)
  for (const s of input.states) if (!inputs.has(s.id)) inputs.set(s.id, s.inputs)
  const blocked = blockedCauses(byId, inputs, input.pinned)
  const healthOf = new Map<string, Health>()
  for (const s of input.states)
    healthOf.set(
      s.id,
      nodeHealth(s, { blocked: blocked.has(s.id), suspect: input.suspect?.has(s.id) ?? false, late: input.late?.has(s.id) ?? false }),
    )

  const nodes = new Map<string, HealthTreeNode>()
  const parent = new Map<string, string>()
  const groups = new Map(input.groups.map((g) => [g.id, g]))
  const grouped = new Set<string>()
  for (const g of input.groups) for (const m of g.members) grouped.add(m)

  const leaf = (id: string): HealthTreeNode | null => {
    const s = byId.get(id)
    const health = healthOf.get(id)
    if (!s || !health) return null
    const counts = zeroCounts()
    counts[health] = 1
    return { id, label: shortName(id), kind: 'node', children: [], leaves: 1, counts, health, weight: healthWeight(s, health), running: s.running ? 1 : 0 }
  }

  const inner = (id: string, label: string, kind: HealthTreeKind, members: readonly string[]): HealthTreeNode => {
    const counts = zeroCounts()
    const node: HealthTreeNode = { id, label, kind, children: [], leaves: 0, counts, health: 'ok', weight: 0, running: 0 }
    for (const m of members) {
      const child = nodes.get(m)
      if (!child) continue
      parent.set(m, id)
      node.children.push(m)
      node.leaves += child.leaves
      node.weight += child.weight
      node.running += child.running
      for (const h of HEALTH_ORDER) counts[h] += child.counts[h]
    }
    node.health = worstHealth(HEALTH_ORDER.filter((h) => counts[h] > 0))
    node.children.sort((a, b) => compareChildren(nodes.get(a)!, nodes.get(b)!))
    nodes.set(id, node)
    return node
  }

  for (const s of input.states) {
    const node = leaf(s.id)
    if (node) nodes.set(s.id, node)
  }

  // Groups bottom-up: a group's members may be groups.
  const built = new Set<string>()
  const buildGroup = (id: string, seen: Set<string>): void => {
    if (built.has(id) || seen.has(id)) return
    const g = groups.get(id)!
    seen.add(id)
    for (const m of g.members) if (groups.has(m)) buildGroup(m, seen)
    inner(id, g.name, 'group', g.members)
    built.add(id)
  }
  for (const id of groups.keys()) buildGroup(id, new Set())

  let top: string[]
  if (groups.size > 0) {
    const roots = input.groups.filter((g) => !grouped.has(g.id)).map((g) => g.id)
    const loose = input.states.map((s) => s.id).filter((id) => !grouped.has(id))
    top = [...roots, ...loose]
  } else {
    const files = new Map<string, string[]>()
    for (const s of input.states) {
      const file = sourceFile(s.id)
      files.set(file, [...(files.get(file) ?? []), s.id])
    }
    top = []
    for (const [file, ids] of files) {
      const id = `file:${file}`
      inner(id, file, 'file', ids)
      top.push(id)
    }
  }
  inner(ROOT, 'All pipelines', 'root', top)
  return { nodes, parent, healthOf, blockedBy: blocked }
}

function compareChildren(a: HealthTreeNode, b: HealthTreeNode): number {
  return healthRank(a.health) - healthRank(b.health) || b.weight - a.weight || b.leaves - a.leaves || a.label.localeCompare(b.label)
}

/** The path from the root to `id`, root first; empty when `id` is not in the tree. */
export function treePath(tree: HealthTree, id: string): HealthTreeNode[] {
  const out: HealthTreeNode[] = []
  let at: string | undefined = id
  while (at !== undefined) {
    const node = tree.nodes.get(at)
    if (!node) return []
    out.unshift(node)
    at = tree.parent.get(at)
  }
  return out
}

/** Every node id underneath `id` (itself, for a node). */
export function leafIds(tree: HealthTree, id: string): string[] {
  const node = tree.nodes.get(id)
  if (!node) return []
  if (node.kind === 'node') return [id]
  return node.children.flatMap((c) => leafIds(tree, c))
}

/** The nearest ancestor of `id` (or itself) that the tree has: where a map focuses. */
export function nearestInTree(tree: HealthTree, id: string | null): string {
  return id && tree.nodes.has(id) ? id : ROOT
}

/** One segment of the sunburst: a tree node, or the members too thin to draw one by one. */
export interface Arc {
  /** The tree node's id; for the thin members of a scope, `<scope>::rest`. */
  id: string
  /** The segment it sits on (or the focus, for the first ring): what a click on `rest` opens. */
  parent: string
  /** 1 is the innermost ring around the centre (the focus is the centre). */
  depth: number
  /** Where it starts and ends around the circle, as fractions of a turn from 12 o'clock. */
  start: number
  end: number
  health: Health
  label: string
  /** `rest`: members of `parent` each too thin to draw, as one quiet segment. */
  kind: HealthTreeKind | 'rest'
  weight: number
  leaves: number
}

/** How much of the circle healthy nodes keep in all, so a healthy group stays visible. */
const HEALTHY_SHARE = 0.15

/**
 * The segments of the map around `focus`, `rings` deep. A segment's angle is its unhealthy
 * weight plus a small per-node floor, so a scope with nothing wrong is a thin slice and the
 * biggest problem is the biggest slice. Members thinner than `minTurn` are drawn together as one
 * segment (`rest`, labelled `N smaller`), as a disk map draws its smaller objects, so the ring
 * stays whole instead of fraying into slivers.
 */
export function sunburst(tree: HealthTree, focus: string, rings = 3, minTurn = 0.01): Arc[] {
  const center = tree.nodes.get(focus)
  if (!center || center.leaves === 0) return []
  const floor = (Math.max(center.weight, 1) * HEALTHY_SHARE) / center.leaves
  const value = (n: HealthTreeNode) => n.weight + floor * n.leaves
  const arcs: Arc[] = []
  const place = (node: HealthTreeNode, start: number, span: number, depth: number) => {
    if (depth > rings || node.children.length === 0) return
    const total = node.children.reduce((sum, c) => sum + value(tree.nodes.get(c)!), 0)
    if (total <= 0) return
    let at = start
    const thin: HealthTreeNode[] = []
    for (const c of node.children) {
      const child = tree.nodes.get(c)!
      const share = (value(child) / total) * span
      if (share < minTurn) {
        thin.push(child)
        continue
      }
      arcs.push({ id: c, parent: node.id, depth, start: at, end: at + share, health: child.health, label: child.label, kind: child.kind, weight: child.weight, leaves: child.leaves })
      place(child, at, share, depth + 1)
      at += share
    }
    if (thin.length > 0) {
      const share = (thin.reduce((sum, n) => sum + value(n), 0) / total) * span
      arcs.push({
        id: `${node.id}::rest`,
        parent: node.id,
        depth,
        start: at,
        end: at + share,
        health: worstHealth(thin.map((n) => n.health)),
        label: `${thin.length.toLocaleString()} smaller`,
        kind: 'rest',
        weight: thin.reduce((sum, n) => sum + n.weight, 0),
        leaves: thin.reduce((sum, n) => sum + n.leaves, 0),
      })
    }
  }
  place(center, 0, 1, 1)
  return arcs
}

/** `3 failing · 169 blocked · 12 behind`: the unhealthy counts of a scope, worst first. */
export function healthSummary(node: Pick<HealthTreeNode, 'counts'>, limit = HEALTH_ORDER.length): string {
  const parts = HEALTH_ORDER.filter((h) => isUnhealthy(h) && node.counts[h] > 0).map((h) => `${node.counts[h].toLocaleString()} ${h === 'suspect' ? 'suspect' : h}`)
  return parts.slice(0, limit).join(' · ')
}

/**
 * An SVG path for a ring sector: from `start` to `end` (fractions of a turn, clockwise from 12
 * o'clock), between radii `r0` and `r1`, centred on (0, 0). `gap` (px) is taken off each side
 * as a constant width at every radius, so neighbouring sectors are parted by even, parallel gaps;
 * a sector thinner than its gaps keeps a hairline.
 */
export function arcPath(start: number, end: number, r0: number, r1: number, gap = 0): string {
  const full = end - start >= 0.9999
  const turn = 2 * Math.PI
  const at = (r: number): [number, number] => {
    if (full) return [start * turn, (start + 0.9999) * turn]
    const pad = r > 0 ? gap / 2 / r : 0
    const a0 = start * turn + pad
    const a1 = end * turn - pad
    if (a1 > a0) return [a0, a1]
    const mid = ((start + end) / 2) * turn
    return [mid - 0.0005, mid + 0.0005]
  }
  const pt = (r: number, a: number) => `${(r * Math.sin(a)).toFixed(3)} ${(-r * Math.cos(a)).toFixed(3)}`
  const [o0, o1] = at(r1)
  const [i0, i1] = at(r0)
  const outer = `M ${pt(r1, o0)} A ${r1} ${r1} 0 ${o1 - o0 > Math.PI ? 1 : 0} 1 ${pt(r1, o1)}`
  const inner = r0 > 0 ? `L ${pt(r0, i1)} A ${r0} ${r0} 0 ${i1 - i0 > Math.PI ? 1 : 0} 0 ${pt(r0, i0)}` : 'L 0 0'
  return `${outer} ${inner} Z`
}
