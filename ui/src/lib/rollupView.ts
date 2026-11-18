/**
 * The rollup as the UI draws it: `GET /state/rollup` indexed by id, and the layout of the health
 * map over it. Nothing here counts, sums or classifies: every count shown is an entry's own
 * (`barca-core`'s `rollup`, the same response `barca status --rollup` prints), in nodes
 * (`nodes`, `health`, `causes`) and in assets (`assets`, `asset_health`). What stays here is
 * geometry (which angle a segment gets, from the entry's served `weight`), the path to an
 * entry, and words.
 */

import { HEALTH_ORDER, isUnhealthy, type Health } from './healthDisplay'
import { MAP_PARTS, keyBandPart, type MapPart } from './causeDisplay'
import type { HeldBy } from './generated/HeldBy'
import { formatSpan, parseTime } from './time'
import type { HealthCounts } from './generated/HealthCounts'
import type { Rollup } from './generated/Rollup'
import type { RollupEntry } from './generated/RollupEntry'
import type { RollupRoot } from './generated/RollupRoot'
import type { EntryKind } from './generated/EntryKind'
import type { DataCoverage } from './generated/DataCoverage'
import type { FileRollup } from './generated/FileRollup'

export type { HealthCounts, HeldBy, Rollup, RollupEntry, RollupRoot, EntryKind }

/** The id of the root entry. */
export const ROOT = 'root'

/** A rollup response, its entries by id. */
export interface RollupTree {
  entries: ReadonlyMap<string, RollupEntry>
  /** What holds the nodes that are not fresh, as served: the most nodes first. */
  roots: readonly RollupRoot[]
  coverage: DataCoverage
  /** Each source file's nodes summed (`files`), by file path. */
  files: ReadonlyMap<string, FileRollup>
  /** Every node in scope by kind (`kinds`): how many of each are fresh and have each cause. */
  kinds: Rollup['kinds'] | null
  asOf: string
}

const NO_COUNTS: HealthCounts = { failing: 0, blocked: 0, suspect: 0, behind: 0, undecided: 0, never_run: 0, ok: 0 }
const NO_CAUSES: RollupEntry['causes'] = { catching_up: 0, queued: 0, no_refresh_path: 0, overdue: 0, failing: 0, undecided: 0 }

/** A tree with only an empty root: what a page holds until the rollup has loaded. */
export const EMPTY_ROLLUP: RollupTree = {
  entries: new Map([[ROOT, { id: ROOT, kind: 'root', label: 'all', parent: null, children: [], nodes: 0, assets: 0, running: 0, health: NO_COUNTS, asset_health: NO_COUNTS, data_coverage: { assets: 0, measured: 0, suspect: 0 }, causes: NO_CAUSES, asset_causes: NO_CAUSES, not_fresh: 0, weight: 0, weights: { ...NO_CAUSES, fresh: 0 }, split: { need_someone: 0, catching_up: 0, run_by_hand: 0, need_someone_nodes: 0, in_hand_nodes: 0, need_someone_weight: 0, in_hand_weight: 0 }, stale_since: null, held_by: [], gap_seconds: 0, unbounded: 0, fresh_by: null, also_in: [] }]]),
  files: new Map(),
  kinds: null,
  roots: [],
  coverage: { assets: 0, measured: 0, suspect: 0 },
  asOf: '',
}

export function indexRollup(rollup: Rollup): RollupTree {
  return { entries: new Map(rollup.entries.map((e) => [e.id, e])), roots: rollup.roots, coverage: rollup.data_coverage, files: new Map(rollup.files.map((f) => [f.file, f])), kinds: rollup.kinds, asOf: rollup.as_of }
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
  const parts = HEALTH_ORDER.filter((h) => isUnhealthy(h) && counts[h] > 0).map((h) => `${counts[h].toLocaleString()} ${h}`)
  return parts.slice(0, limit).join(' · ')
}

/**
 * `fresh within about 5m`: when the nodes of a scope that have an estimate are all expected
 * fresh (`fresh_by`), from the instant the rows were classified (`as_of`), rounded up to the
 * unit shown so an estimate is never said to be sooner. Null when no node has an estimate.
 */
export function freshWithin(entry: Pick<RollupEntry, 'fresh_by'>, asOf: string): string | null {
  const at = entry.fresh_by === null ? null : parseTime(entry.fresh_by)
  const now = parseTime(asOf)
  if (at === null || now === null) return null
  const left = Math.max(0, Math.ceil((at - now) / 1000))
  if (left === 0) return 'freshness estimate due now'
  const unit = left < 60 ? 1 : left < 3600 ? 60 : left < 86400 ? 3600 : 86400
  return `fresh within about ${formatSpan(Math.ceil(left / unit) * unit)}`
}

/** `399 with no estimate`: the not-fresh nodes of a scope nothing known will leave fresh. */
export function noEstimate(entry: Pick<RollupEntry, 'unbounded'>): string | null {
  return entry.unbounded > 0 ? `${entry.unbounded.toLocaleString()} with no estimate` : null
}

/** One colour of a segment: a cause, or fresh, over the part of the segment its nodes take. */
export interface Band {
  part: MapPart
  /** Where it starts and ends around the circle, inside its segment. */
  start: number
  end: number
}

/** One segment of the sunburst: an entry, the members too thin to draw one by one, or keys. */
export interface Arc {
  /**
   * The entry's id; for the thin members of a scope, `<scope>::rest`; for a partitioned node's
   * keys, `<node>::keys:<failed|behind|ok>`.
   */
  id: string
  /** The segment it sits on (or the focus, for the first ring): what a click on `rest` opens. */
  parent: string
  /** 1 is the innermost ring around the centre (the focus is the centre). */
  depth: number
  /** Where it starts and ends around the circle, as fractions of a turn from 12 o'clock. */
  start: number
  end: number
  label: string
  /**
   * `rest`: the members of `parent` too thin to draw, as one segment.
   * `keys`: a partitioned node's keys that stand one way (failed, not up to date, ok).
   */
  kind: EntryKind | 'rest' | 'keys'
  /**
   * Its colours, stacked in the served order of the causes, then fresh: each as wide as the
   * nodes of that colour underneath weigh, so a group reads as what is inside it, never as
   * its worst member. One band for a node or for keys.
   */
  bands: Band[]
  /** For `rest`: the members drawn together. */
  members?: readonly string[]
  /** For `keys`: how many keys, and of how many the node has. */
  keys?: { count: number; of: number }
}

/** The keys ring's three bands, and what each says. */
const KEY_BANDS: readonly (readonly ['failed' | 'behind' | 'ok', string])[] = [
  ['failed', 'failed'],
  ['behind', 'not up to date'],
  ['ok', 'ok'],
]

/** What a map entry is laid out by: its distance from fresh, as served. */
type Weighed = Pick<RollupEntry, 'weight' | 'weights'>

/**
 * The served weight of each colour inside an entry (`weights`), in stacking order, the empty
 * ones left out. How much room an entry takes is its `weight`: its distance from fresh in
 * weighted nodes, which `barca-core` computes ("Distance from fresh": every node counts 1, a
 * node that is not fresh its cause's weight) and `barca status --rollup` prints. A level's is
 * exactly the sum of its children's. No weight is decided here.
 */
function partWeights(entry: Weighed): [MapPart, number][] {
  return MAP_PARTS.map((part): [MapPart, number] => [part, entry.weights[part]]).filter(([, w]) => w > 0)
}

/** `weights` laid side by side over a segment from `start`, `span` wide. */
function bandsOver(weights: readonly [MapPart, number][], start: number, span: number): Band[] {
  const total = weights.reduce((sum, [, w]) => sum + w, 0)
  let at = start
  return weights.map(([part, w]) => {
    const band = { part, start: at, end: at + (w / total) * span }
    at = band.end
    return band
  })
}

/**
 * The segments of the map around `focus`, `rings` deep. A segment's angle is its entry's weight
 * (the served `weight`: how far what is underneath is from fresh), so the room a group takes is how big
 * its problem is, and a fresh group is a thin calm sliver, not gone. Its colours are what is
 * inside it, stacked by weight: failing, overdue, no refresh path, queued, undecided, catching
 * up, fresh. Children come worst first (the served order), so the trouble sits clockwise from
 * 12 o'clock. Members thinner than `minTurn` are drawn together after them (`rest`, labelled
 * `N smaller`): one segment as wide as they are, stacked the same way, so the ring stays whole
 * instead of fraying into slivers. Outside a partitioned node with room for it is a ring of its
 * keys: failed, not up to date, ok.
 */
export function sunburst(tree: RollupTree, focus: string, rings = 3, minTurn = 0.01): Arc[] {
  const center = tree.entries.get(focus)
  if (!center || center.nodes === 0) return []
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
    let at = start
    bands.forEach(([band, word], i) => {
      const count = keys[band]
      // Keys not up to date take the colour of the cause the server gives them (`keys.cause`).
      const part = keyBandPart(band, keys.cause)
      const label = `${count.toLocaleString()} ${count === 1 ? 'key' : 'keys'} ${word}`
      const end = at + widths[i]!
      arcs.push({ id: `${entry.id}::keys:${band}`, parent: entry.id, depth, start: at, end, label, kind: 'keys', bands: [{ part, start: at, end }], keys: { count, of } })
      at = end
    })
  }
  const place = (entry: RollupEntry, start: number, span: number, depth: number) => {
    if (depth > rings) return
    if (entry.children.length === 0) return keyRing(entry, start, span, depth)
    const total = entry.weight
    if (total <= 0) return
    let at = start
    const thin: RollupEntry[] = []
    for (const c of entry.children) {
      const child = tree.entries.get(c)
      if (!child || child.nodes === 0) continue
      const share = (child.weight / total) * span
      if (share < minTurn) {
        thin.push(child)
        continue
      }
      arcs.push({ id: c, parent: entry.id, depth, start: at, end: at + share, label: child.label, kind: child.kind, bands: bandsOver(partWeights(child), at, share) })
      place(child, at, share, depth + 1)
      at += share
    }
    if (thin.length === 0) return
    // The thin members, last: one segment, each colour as wide as it would have been across
    // them, so a run of small groups that are mostly fine reads mostly green.
    const weights = MAP_PARTS.map((part): [MapPart, number] => [part, thin.reduce((sum, child) => sum + (partWeights(child).find(([p]) => p === part)?.[1] ?? 0), 0)]).filter(([, w]) => w > 0)
    const share = (weights.reduce((sum, [, w]) => sum + w, 0) / total) * span
    arcs.push({ id: `${entry.id}::rest`, parent: entry.id, depth, start: at, end: at + share, label: `${thin.length.toLocaleString()} smaller`, kind: 'rest', bands: bandsOver(weights, at, share), members: thin.map((t) => t.id) })
  }
  place(center, 0, 1, 1)
  return arcs
}

/**
 * What a root holds, as ids the map and the table can light: every entry whose `held_by` names
 * the root (the scopes with a node it holds), and every node whose row names it (`root.id` of
 * `GET /state`). Both are the server's attribution; this only collects the ids.
 */
export function heldIds(tree: RollupTree, rows: Iterable<{ id: string; root: { id: string } | null }>, root: string): Set<string> {
  const out = new Set<string>()
  for (const entry of tree.entries.values()) if (entry.held_by.some((h) => h.root === root)) out.add(entry.id)
  for (const row of rows) if (row.root?.id === root) out.add(row.id)
  return out
}

/**
 * An SVG path for a ring sector: from `start` to `end` (fractions of a turn, clockwise from 12
 * o'clock), between radii `r0` and `r1`, centred on (0, 0). `gap` (px) is taken off each side
 * as a constant width at every radius, so neighbouring sectors are parted by even, parallel gaps;
 * a sector thinner than its gaps keeps a hairline. `sides` says which of its two sides give the
 * gap up: a colour band inside a segment touches its neighbour band.
 */
export function arcPath(start: number, end: number, r0: number, r1: number, gap = 0, sides: readonly [boolean, boolean] = [true, true]): string {
  const full = end - start >= 0.9999
  const turn = 2 * Math.PI
  const at = (r: number): [number, number] => {
    if (full) return [start * turn, (start + 0.9999) * turn]
    const pad = r > 0 ? gap / 2 / r : 0
    const a0 = start * turn + (sides[0] ? pad : 0)
    const a1 = end * turn - (sides[1] ? pad : 0)
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
