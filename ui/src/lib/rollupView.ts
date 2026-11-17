/**
 * The rollup as the UI draws it: `GET /state/rollup` indexed by id, and the layout of the health
 * map over it. Nothing here counts, sums or classifies: every count is an entry's own
 * (`barca-core`'s `rollup`, the same response `barca status --rollup` prints), in nodes
 * (`nodes`, `health`) and in assets (`assets`, `asset_health`). What stays here is geometry
 * (which angle a segment gets), the path to an entry, and words.
 */

import { HEALTH_ORDER, healthFamily, isUnhealthy, type Health, type HealthFamily } from './healthDisplay'
import type { HealthCounts } from './generated/HealthCounts'
import type { Rollup } from './generated/Rollup'
import type { RollupEntry } from './generated/RollupEntry'
import type { RollupRoot } from './generated/RollupRoot'
import type { EntryKind } from './generated/EntryKind'
import type { DataCoverage } from './generated/DataCoverage'

export type { HealthCounts, Rollup, RollupEntry, RollupRoot, EntryKind }

/** The id of the root entry. */
export const ROOT = 'root'

/** What a count counts: every display names the one it shows. */
export type Unit = 'nodes' | 'assets'

/** A rollup response, its entries by id. */
export interface RollupTree {
  entries: ReadonlyMap<string, RollupEntry>
  /** What holds the nodes that are not fresh, as served: the most nodes first. */
  roots: readonly RollupRoot[]
  coverage: DataCoverage
  asOf: string
}

const NO_COUNTS: HealthCounts = { failing: 0, blocked: 0, suspect: 0, behind: 0, undecided: 0, never_run: 0, ok: 0 }

/** A tree with only an empty root: what a page holds until the rollup has loaded. */
export const EMPTY_ROLLUP: RollupTree = {
  entries: new Map([[ROOT, { id: ROOT, kind: 'root', label: 'all', parent: null, children: [], nodes: 0, assets: 0, running: 0, health: NO_COUNTS, asset_health: NO_COUNTS, data_coverage: { assets: 0, measured: 0, suspect: 0 } }]]),
  roots: [],
  coverage: { assets: 0, measured: 0, suspect: 0 },
  asOf: '',
}

export function indexRollup(rollup: Rollup): RollupTree {
  return { entries: new Map(rollup.entries.map((e) => [e.id, e])), roots: rollup.roots, coverage: rollup.data_coverage, asOf: rollup.as_of }
}

/** The most severe health an entry counts (the served order); `ok` for an empty one. */
export function worstOf(counts: HealthCounts): Health {
  return HEALTH_ORDER.find((h) => counts[h] > 0) ?? 'ok'
}

/** The path from the root to `id`, root first; empty when `id` is not in the tree. */
export function treePath(tree: RollupTree, id: string): RollupEntry[] {
  const out: RollupEntry[] = []
  let at: string | null = id
  while (at !== null) {
    const entry = tree.entries.get(at)
    if (!entry) return []
    out.unshift(entry)
    at = entry.parent
  }
  return out
}

/** Every node id underneath `id` (itself, for a node). */
export function leafIds(tree: RollupTree, id: string): string[] {
  const entry = tree.entries.get(id)
  if (!entry) return []
  if (entry.kind === 'node') return [id]
  return entry.children.flatMap((c) => leafIds(tree, c))
}

/** `id` when the tree has it, else the root: where a map focuses. */
export function nearestInTree(tree: RollupTree, id: string | null): string {
  return id && tree.entries.has(id) ? id : ROOT
}

/** The roots about nodes underneath `id`, in served order. */
export function rootsWithin(tree: RollupTree, id: string): RollupRoot[] {
  if (id === ROOT) return [...tree.roots]
  const within = new Set(leafIds(tree, id))
  return tree.roots.filter((r) => within.has(r.node))
}

/** `3 failing · 169 blocked · 12 behind`: the unhealthy counts of a scope, worst first. */
export function healthSummary(counts: HealthCounts, limit = HEALTH_ORDER.length): string {
  const parts = HEALTH_ORDER.filter((h) => isUnhealthy(h) && counts[h] > 0).map((h) => `${counts[h].toLocaleString()} ${h === 'suspect' ? 'suspect' : h}`)
  return parts.slice(0, limit).join(' · ')
}

/** One segment of the sunburst: an entry, the members too thin to draw one by one, or keys. */
export interface Arc {
  /**
   * The entry's id; for the thin members of a scope, `<scope>::rest:<family>`; for a
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
   * `rest`: the members of `parent` too thin to draw, one band per colour.
   * `keys`: a partitioned node's keys that stand one way (failed, not up to date, ok).
   */
  kind: EntryKind | 'rest' | 'keys'
  /** What its angle is, in the map's unit: assets underneath, or nodes in a scope with no assets (0 for keys). */
  size: number
  /** For `keys`: how many keys, and of how many the node has. */
  keys?: { count: number; of: number }
}

/** The keys ring's three bands: what each says, and the health it is drawn as. */
const KEY_BANDS: readonly (readonly ['failed' | 'behind' | 'ok', string])[] = [
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

/** The unit a map around `center` is sized in: assets, or nodes where there is no asset at all. */
export function mapUnit(center: Pick<RollupEntry, 'assets'>): Unit {
  return center.assets > 0 ? 'assets' : 'nodes'
}

/**
 * The segments of the map around `focus`, `rings` deep. A segment's angle is how many assets the
 * entry says are underneath (tasks and sensors colour their group but take no room, unless the
 * scope has no assets at all), so the map is the project's shape; its colour is the worst health
 * the entry counts, so trouble shows from the centre. Children come worst first (the served
 * order), so the trouble sits clockwise from 12 o'clock. Members thinner than `minTurn` are drawn
 * together after them (`rest`, labelled `N smaller`): one band per colour, as wide as their
 * angles in that colour, so the ring stays whole instead of fraying into slivers. Outside a
 * partitioned node with room for it is a ring of its keys: failed, not up to date, ok.
 */
export function sunburst(tree: RollupTree, focus: string, rings = 3, minTurn = 0.01): Arc[] {
  const center = tree.entries.get(focus)
  if (!center || center.nodes === 0) return []
  const unit = mapUnit(center)
  const sizeOf = (e: RollupEntry) => (unit === 'assets' ? e.assets : e.nodes)
  const countsOf = (e: RollupEntry) => (unit === 'assets' ? e.asset_health : e.health)
  const arcs: Arc[] = []
  const keyRing = (entry: RollupEntry, start: number, span: number, depth: number) => {
    const keys = entry.keys
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
    const own = worstOf(entry.health)
    let at = start
    bands.forEach(([band, word], i) => {
      const count = keys[band]
      const health: Health = band === 'failed' ? 'failing' : band === 'ok' ? 'ok' : behindAs(own)
      const label = `${count.toLocaleString()} ${count === 1 ? 'key' : 'keys'} ${word}`
      arcs.push({ id: `${entry.id}::keys:${band}`, parent: entry.id, depth, start: at, end: at + widths[i]!, health, label, kind: 'keys', size: 0, keys: { count, of } })
      at += widths[i]!
    })
  }
  const place = (entry: RollupEntry, start: number, span: number, depth: number) => {
    if (depth > rings) return
    if (entry.children.length === 0) return keyRing(entry, start, span, depth)
    const total = sizeOf(entry)
    if (total <= 0) return
    let at = start
    const thin: RollupEntry[] = []
    for (const c of entry.children) {
      const child = tree.entries.get(c)
      if (!child || sizeOf(child) === 0) continue
      const share = (sizeOf(child) / total) * span
      if (share < minTurn) {
        thin.push(child)
        continue
      }
      arcs.push({ id: c, parent: entry.id, depth, start: at, end: at + share, health: worstOf(child.health), label: child.label, kind: child.kind, size: sizeOf(child) })
      place(child, at, share, depth + 1)
      at += share
    }
    // The thin members, last: one band per colour, as wide as the angles they would have had in
    // that colour, so a run of small groups that are mostly fine reads mostly green.
    for (const family of FAMILY_ORDER) {
      const healths = HEALTH_ORDER.filter((h) => healthFamily(h) === family)
      let size = 0
      let worst: Health | null = null
      for (const child of thin)
        for (const h of healths) {
          const n = countsOf(child)[h]
          if (n === 0) continue
          size += n
          if (worst === null || HEALTH_ORDER.indexOf(h) < HEALTH_ORDER.indexOf(worst)) worst = h
        }
      if (size === 0 || worst === null) continue
      const share = (size / total) * span
      arcs.push({ id: `${entry.id}::rest:${family}`, parent: entry.id, depth, start: at, end: at + share, health: worst, label: `${thin.length.toLocaleString()} smaller`, kind: 'rest', size })
      at += share
    }
  }
  place(center, 0, 1, 1)
  return arcs
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
