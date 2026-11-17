/**
 * Asset table filters — pure, URL-backed.
 *
 * Every filter narrows the rows (they combine with AND; a multi-select group
 * matches any of its picks). Chip counts are faceted: each group's counts
 * apply every *other* active filter, so a count is always what clicking that
 * chip would show. The state chips are the health map's parts: a row falls under
 * its served `cause` (or `fresh`), and with no other filter their counts are the
 * rollup's own.
 */

import { match } from 'ts-pattern'
import { filterRows, type AssetRow } from './assetTable'
import { MAP_PARTS, mapPartOf, type MapPart } from './causeDisplay'
import type { KindCounts } from './generated/KindCounts'
import type { NodeState } from './types'

export type Kind = NodeState['kind']
const KINDS: readonly Kind[] = ['asset', 'task', 'sensor']

export type LastRun = 'any' | 'hour' | 'day' | 'week_plus' | 'never'
export const LAST_RUNS: readonly LastRun[] = ['any', 'hour', 'day', 'week_plus', 'never']

export interface Filters {
  query: string
  /**
   * Empty = any. The health map's parts: a cause (`failing`, `overdue`, `no_refresh_path`,
   * `queued`, `undecided`, `catching_up`) or `fresh`. A row falls under its served `cause`.
   */
  states: MapPart[]
  /** Only pinned nodes (`barca pin`): a fact beside the cause, so a filter of its own. */
  pinned: boolean
  /** Empty = any kind. */
  kinds: Kind[]
  lastRun: LastRun
  /** Only nodes with a cron schedule. */
  scheduled: boolean
  /** Only partitioned nodes. */
  partitioned: boolean
}

export const EMPTY_FILTERS: Filters = {
  query: '',
  states: [],
  pinned: false,
  kinds: [],
  lastRun: 'any',
  scheduled: false,
  partitioned: false,
}

export function isFiltered(f: Filters): boolean {
  return (
    f.query.trim() !== '' ||
    f.states.length > 0 ||
    f.pinned ||
    f.kinds.length > 0 ||
    f.lastRun !== 'any' ||
    f.scheduled ||
    f.partitioned
  )
}

const HOUR = 3_600_000
const DAY = 24 * HOUR

function matchesLastRun(row: AssetRow, lastRun: LastRun, nowMs: number): boolean {
  const at = row.lastAtMs
  return match(lastRun)
    .with('any', () => true)
    .with('hour', () => at !== null && nowMs - at <= HOUR)
    .with('day', () => at !== null && nowMs - at <= DAY)
    .with('week_plus', () => at !== null && nowMs - at > 7 * DAY)
    .with('never', () => at === null)
    .exhaustive()
}

type Group = 'states' | 'pinned' | 'kinds' | 'lastRun' | 'scheduled' | 'partitioned'

/** The state filter: the row's own served cause (or fresh) is one of the parts picked. */
function matchesStates(r: AssetRow, f: Filters): boolean {
  return f.states.length === 0 || f.states.includes(mapPartOf(r.cause))
}

/** Rows passing every filter except those of `skip` (for faceted counts). */
function filterExcept(rows: AssetRow[], f: Filters, nowMs: number, skip?: Group): AssetRow[] {
  return filterRows(rows, f.query).filter(
    (r) =>
      (skip === 'states' || matchesStates(r, f)) &&
      (skip === 'pinned' || !f.pinned || r.pinned !== null) &&
      (skip === 'kinds' || f.kinds.length === 0 || f.kinds.includes(r.kind)) &&
      (skip === 'lastRun' || matchesLastRun(r, f.lastRun, nowMs)) &&
      (skip === 'scheduled' || !f.scheduled || r.nextRunMs !== null) &&
      (skip === 'partitioned' || !f.partitioned || r.partitioned),
  )
}

export function applyFilters(rows: AssetRow[], f: Filters, nowMs: number): AssetRow[] {
  return filterExcept(rows, f, nowMs)
}

interface FacetCounts {
  /** Rows by the health map's part: each cause, and fresh. */
  states: Record<MapPart, number>
  /** Pinned rows. */
  pinned: number
  kinds: Record<Kind, number>
  lastRun: Record<LastRun, number>
  scheduled: number
  partitioned: number
}

/** Whether any filter but the state chips narrows the rows: a search, a kind, a time, a toggle. */
function narrowed(f: Filters): boolean {
  return isFiltered({ ...f, states: [] })
}

/**
 * The chips' counts. `served` is the page's kind in the rollup (`kinds` of `GET /state/rollup`,
 * or of the open file): with it, and no filter but the state chips, a chip's count is the
 * server's own (`fresh`, `causes`), the number `barca status --rollup --json` prints. Under a
 * search or another filter a count is the rows on screen that carry that cause: a count of
 * what is listed, by a field each row was served with. Nothing here decides which chip a row
 * falls under.
 */
export function facetCounts(rows: AssetRow[], f: Filters, nowMs: number, served: KindCounts | null = null): FacetCounts {
  const states = Object.fromEntries(MAP_PARTS.map((p) => [p, 0])) as Record<MapPart, number>
  if (served && !narrowed(f)) {
    for (const part of MAP_PARTS) states[part] = part === 'fresh' ? served.fresh : served.causes[part]
  } else {
    for (const r of filterExcept(rows, f, nowMs, 'states')) states[mapPartOf(r.cause)] += 1
  }
  const pinned = filterExcept(rows, f, nowMs, 'pinned').filter((r) => r.pinned !== null).length

  const kinds = { asset: 0, task: 0, sensor: 0 } as Record<Kind, number>
  for (const r of filterExcept(rows, f, nowMs, 'kinds')) kinds[r.kind] += 1

  const forLastRun = filterExcept(rows, f, nowMs, 'lastRun')
  const lastRun = Object.fromEntries(
    LAST_RUNS.map((l) => [l, forLastRun.filter((r) => matchesLastRun(r, l, nowMs)).length]),
  ) as Record<LastRun, number>

  return {
    states,
    pinned,
    kinds,
    lastRun,
    scheduled: filterExcept(rows, f, nowMs, 'scheduled').filter((r) => r.nextRunMs !== null)
      .length,
    partitioned: filterExcept(rows, f, nowMs, 'partitioned').filter((r) => r.partitioned).length,
  }
}

// ── URL ───────────────────────────────────────────────────────────────────────

const PARAMS = ['q', 'state', 'failed', 'blocked', 'pinned', 'kind', 'ran', 'scheduled', 'partitioned'] as const

/** `state=` values of links made before the chips spoke of causes, and the part each now means. */
const OLD_STATES: Readonly<Record<string, MapPart>> = { cached: 'fresh' }

function list<T extends string>(raw: string | null, allowed: readonly T[]): T[] {
  if (!raw) return []
  return raw.split(',').filter((v): v is T => (allowed as readonly string[]).includes(v))
}

/**
 * The filters a URL names; one that names none filters nothing, on every page (what the page
 * shows is what its controls say). `state=` carries the health map's parts (the server's cause
 * words, and `fresh`). An older link's `failed=1` or `blocked=1` reads `state=failing`: both
 * are the `failing` cause now.
 */
export function parseFilters(p: URLSearchParams): Filters {
  const ran = p.get('ran')
  return {
    query: p.get('q') ?? '',
    states: [...new Set<MapPart>([
      ...list((p.get('state') ?? '').split(',').map((s) => OLD_STATES[s] ?? s).join(','), MAP_PARTS),
      ...(p.get('failed') === '1' || p.get('blocked') === '1' ? (['failing'] as const) : []),
    ])],
    pinned: p.get('pinned') === '1',
    kinds: list(p.get('kind'), KINDS),
    lastRun: LAST_RUNS.find((l) => l === ran) ?? 'any',
    scheduled: p.get('scheduled') === '1',
    partitioned: p.get('partitioned') === '1',
  }
}

/** `params` with the filter keys replaced by `f` (other keys are kept). */
export function writeFilters(params: URLSearchParams, f: Filters): URLSearchParams {
  const p = new URLSearchParams(params)
  for (const k of PARAMS) p.delete(k)
  if (f.query.trim()) p.set('q', f.query)
  if (f.states.length) p.set('state', f.states.join(','))
  if (f.pinned) p.set('pinned', '1')
  if (f.kinds.length) p.set('kind', f.kinds.join(','))
  if (f.lastRun !== 'any') p.set('ran', f.lastRun)
  if (f.scheduled) p.set('scheduled', '1')
  if (f.partitioned) p.set('partitioned', '1')
  return p
}
