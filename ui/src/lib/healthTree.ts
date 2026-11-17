/**
 * The project as one health tree: root → group → … → node, every level the exact sum of its
 * children, so no two screens can disagree about a scope. The health map draws it as a
 * sunburst whose segments are sized by how many assets are underneath and coloured by the worst
 * health inside; the causes list and the tables read their counts from the same tree.
 *
 * With groups (`group(...)`), the top level is the root groups plus the nodes in no group; a
 * project without groups is shown by source file. Pure: no React, no fetching.
 */

import { HEALTH_ORDER, healthFamily, healthRank, healthWeight, isUnhealthy, worstHealth, type Health, type HealthFamily } from './healthDisplay'
import { shortName } from './graph'
import { sourceFile } from './pipeline'
import type { NodeGroup, NodeState } from './types'

export const ROOT = '__root__'

export type HealthTreeKind = 'root' | 'group' | 'file' | 'node'

export interface HealthTreeNode {
  id: string
  label: string
  kind: HealthTreeKind
  /** Children, worst first, then the biggest, then by name. */
  children: string[]
  /** Nodes underneath (itself, for a node). */
  leaves: number
  /** Nodes underneath by health. */
  counts: Record<Health, number>
  /** Assets underneath (tasks and sensors are nodes but not assets): what the map sizes by. */
  assets: number
  /** Assets underneath by health. */
  assetCounts: Record<Health, number>
  /** The worst health underneath. */
  health: Health
  /** How much is wrong underneath: unhealthy keys of partitioned nodes, else nodes. */
  weight: number
  /** Nodes underneath a run is working on now. */
  running: number
  /** A partitioned node's keys, by how they stand (a node only). */
  keys?: KeyCounts
}

/** A partitioned node's keys: failed, not up to date (stale or never run), and ok. */
export interface KeyCounts {
  failed: number
  behind: number
  ok: number
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
}

/**
 * Which failing node each blocked node is counted under: the nearest node upstream whose
 * `health` is `failing`, through inputs that are `blocked` themselves. It reads the server's
 * words only (who is failing, who is blocked) and attributes one to the other for the causes
 * list; the server's `root` field (issue 963) replaces it.
 */
function blockedAttribution(byId: ReadonlyMap<string, NodeState>, inputs: ReadonlyMap<string, readonly string[]>): Map<string, string> {
  const known = new Map<string, string | null>()
  const visiting = new Set<string>()
  const failingAbove = (id: string): string | null => {
    if (known.has(id)) return known.get(id)!
    if (visiting.has(id)) return null
    visiting.add(id)
    let found: string | null = null
    for (const input of inputs.get(id) ?? []) {
      const health = byId.get(input)?.health
      if (health === 'failing') found = input
      else if (health === 'blocked') found = failingAbove(input)
      if (found) break
    }
    visiting.delete(id)
    known.set(id, found)
    return found
  }
  const out = new Map<string, string>()
  for (const [id, node] of byId) {
    if (node.health_reason !== 'upstream_failing') continue
    const by = failingAbove(id)
    if (by) out.set(id, by)
  }
  return out
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
  const healthOf = new Map<string, Health>()
  for (const s of input.states) healthOf.set(s.id, s.health)
  const blocked = blockedAttribution(byId, inputs)

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
    const p = s.partitioned ? s.partitions : null
    const failed = p?.failed ?? 0
    const keys = p && p.total > 0 ? { failed, behind: Math.max(0, p.total - p.cached - failed), ok: p.cached } : undefined
    const assets = s.kind === 'asset' ? 1 : 0
    const assetCounts = zeroCounts()
    assetCounts[health] = assets
    return { id, label: shortName(id), kind: 'node', children: [], leaves: 1, counts, assets, assetCounts, health, weight: healthWeight(s, health), running: s.running ? 1 : 0, keys }
  }

  const inner = (id: string, label: string, kind: HealthTreeKind, members: readonly string[]): HealthTreeNode => {
    const counts = zeroCounts()
    const assetCounts = zeroCounts()
    const node: HealthTreeNode = { id, label, kind, children: [], leaves: 0, counts, assets: 0, assetCounts, health: 'ok', weight: 0, running: 0 }
    for (const m of members) {
      const child = nodes.get(m)
      if (!child) continue
      parent.set(m, id)
      node.children.push(m)
      node.leaves += child.leaves
      node.assets += child.assets
      node.weight += child.weight
      node.running += child.running
      for (const h of HEALTH_ORDER) {
        counts[h] += child.counts[h]
        assetCounts[h] += child.assetCounts[h]
      }
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
  return healthRank(a.health) - healthRank(b.health) || b.assets - a.assets || b.leaves - a.leaves || b.weight - a.weight || a.label.localeCompare(b.label)
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

/** One segment of the sunburst: a tree node, the members too thin to draw one by one, or keys. */
export interface Arc {
  /**
   * The tree node's id; for the thin members of a scope, `<scope>::rest:<family>`; for a
   * partitioned node's keys, `<node>::keys:<failed|behind|ok>`.
   */
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
  /**
   * `rest`: the nodes of one colour inside the members of `parent` too thin to draw.
   * `keys`: a partitioned node's keys that stand one way (failed, not up to date, ok).
   */
  kind: HealthTreeKind | 'rest' | 'keys'
  weight: number
  /** Nodes underneath (0 for keys). */
  leaves: number
  /** What its angle is: assets underneath, or nodes in a scope with no assets (0 for keys). */
  size: number
  /** For `keys`: how many keys, and of how many the node has. */
  keys?: { count: number; of: number }
}

/** The keys ring's three bands: what each says, and the health it is drawn as. */
const KEY_BANDS: readonly (readonly [keyof KeyCounts, string])[] = [
  ['failed', 'failed'],
  ['behind', 'not up to date'],
  ['ok', 'ok'],
]

/** The order the thin members' bands are drawn in: worst first. */
const FAMILY_ORDER: readonly HealthFamily[] = ['failing', 'attention', 'unknown', 'ok']

/** Keys not up to date take the node's own word (blocked, never run, …), else `behind`. */
function behindAs(health: Health): Health {
  return health === 'failing' || health === 'ok' ? 'behind' : health
}

/**
 * The segments of the map around `focus`, `rings` deep. A segment's angle is how many assets are
 * underneath (tasks and sensors colour their group but take no room, unless the scope has no
 * assets at all), so the map is the project's shape and a group that is mostly green looks mostly
 * green one ring out; its colour is the worst health inside, so trouble shows from the centre.
 * Members are worst first, so the trouble sits clockwise from 12 o'clock. Members thinner than
 * `minTurn` are drawn together after them (`rest`, labelled `N smaller`), as a disk map draws its
 * smaller objects, so the ring stays whole instead of fraying into slivers: one band per colour,
 * sized by the assets inside them in that colour. Outside
 * a partitioned node with room for it is a ring of its keys: failed, not up to date, ok.
 */
export function sunburst(tree: HealthTree, focus: string, rings = 3, minTurn = 0.01): Arc[] {
  const center = tree.nodes.get(focus)
  if (!center || center.leaves === 0) return []
  // Assets, so the map agrees with the page's counts; nodes only where there is no asset at all.
  const byAssets = center.assets > 0
  const sizeOf = (n: HealthTreeNode) => (byAssets ? n.assets : n.leaves)
  const countsOf = (n: HealthTreeNode) => (byAssets ? n.assetCounts : n.counts)
  const arcs: Arc[] = []
  const keyRing = (node: HealthTreeNode, start: number, span: number, depth: number) => {
    const keys = node.keys
    if (!keys || depth > rings || span < minTurn * 2) return
    const of = keys.failed + keys.behind + keys.ok
    // A band with keys in it is never thinner than half `minTurn`, so 24 failed keys of 5,000
    // stay visible; the others give up the difference in proportion.
    const bands = KEY_BANDS.filter(([band]) => keys[band] > 0)
    const floor = Math.min(minTurn / 2, span / bands.length)
    const raw = bands.map(([band]) => Math.max((keys[band] / of) * span, floor))
    const extra = raw.reduce((sum, w) => sum + w, 0) - span
    const roomy = raw.reduce((sum, w) => sum + (w > floor ? w - floor : 0), 0)
    const widths = raw.map((w) => (w > floor && roomy > 0 ? w - (extra * (w - floor)) / roomy : w))
    let at = start
    bands.forEach(([band, word], i) => {
      const count = keys[band]
      const health: Health = band === 'failed' ? 'failing' : band === 'ok' ? 'ok' : behindAs(node.health)
      const label = `${count.toLocaleString()} ${count === 1 ? 'key' : 'keys'} ${word}`
      arcs.push({ id: `${node.id}::keys:${band}`, parent: node.id, depth, start: at, end: at + widths[i]!, health, label, kind: 'keys', weight: 0, leaves: 0, size: 0, keys: { count, of } })
      at += widths[i]!
    })
  }
  const place = (node: HealthTreeNode, start: number, span: number, depth: number) => {
    if (depth > rings) return
    if (node.children.length === 0) return keyRing(node, start, span, depth)
    const total = node.children.reduce((sum, c) => sum + sizeOf(tree.nodes.get(c)!), 0)
    if (total <= 0) return
    let at = start
    const thin: HealthTreeNode[] = []
    for (const c of node.children) {
      const child = tree.nodes.get(c)!
      if (sizeOf(child) === 0) continue
      const share = (sizeOf(child) / total) * span
      if (share < minTurn) {
        thin.push(child)
        continue
      }
      arcs.push({ id: c, parent: node.id, depth, start: at, end: at + share, health: child.health, label: child.label, kind: child.kind, weight: child.weight, leaves: child.leaves, size: sizeOf(child) })
      place(child, at, share, depth + 1)
      at += share
    }
    // The thin members, last: one band per colour, sized by the assets inside them in that
    // colour, so a run of small groups that are mostly fine reads mostly green.
    const counts = zeroCounts()
    const nodes = zeroCounts()
    for (const n of thin) {
      for (const h of HEALTH_ORDER) {
        counts[h] += countsOf(n)[h]
        nodes[h] += n.counts[h]
      }
    }
    for (const family of FAMILY_ORDER) {
      const healths = HEALTH_ORDER.filter((h) => healthFamily(h) === family && counts[h] > 0)
      const size = healths.reduce((sum, h) => sum + counts[h], 0)
      if (size === 0) continue
      const share = (size / total) * span
      arcs.push({
        id: `${node.id}::rest:${family}`,
        parent: node.id,
        depth,
        start: at,
        end: at + share,
        health: worstHealth(healths),
        label: `${thin.length.toLocaleString()} smaller`,
        kind: 'rest',
        weight: 0,
        leaves: healths.reduce((sum, h) => sum + nodes[h], 0),
        size,
      })
      at += share
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

/** How a map's rings are sized: the first ring's largest width (a fraction of the outer radius),
 * how much each ring out keeps of the one inside it, and the gap between rings (px). */
export interface RingShape {
  firstMax: number
  decay: number
  gap: number
}

/**
 * Each ring's inner and outer radius, for `rings` levels outside a hole of radius `hole`, out to
 * `outer`: each ring `decay` times as wide as the one inside it, so deep levels are fine texture
 * and the near ones carry the shape, the whole filling the space (the first ring never wider than
 * `firstMax` of the radius, so one level does not become a thick band).
 */
export function ringRadii(rings: number, hole: number, outer: number, shape: RingShape): (readonly [number, number])[] {
  if (rings <= 0) return []
  const span = outer - hole - shape.gap * rings
  const sum = shape.decay === 1 ? rings : (1 - shape.decay ** rings) / (1 - shape.decay)
  const first = Math.min(span / sum, outer * shape.firstMax)
  const out: (readonly [number, number])[] = []
  let at = hole + shape.gap
  for (let k = 0; k < rings; k++) {
    const width = first * shape.decay ** k
    out.push([at, at + width] as const)
    at += width + shape.gap
  }
  return out
}
