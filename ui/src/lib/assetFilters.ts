/**
 * Asset table filters — pure, URL-backed.
 *
 * Every filter narrows the rows (they combine with AND; a multi-select group
 * matches any of its picks). Chip counts are faceted: each group's counts
 * apply every *other* active filter, so a count is always what clicking that
 * chip would show.
 */

import { match } from 'ts-pattern'
import { SEVERITIES, filterRows, type AssetRow, type Severity } from './assetTable'
import type { NodeState } from './types'

export type Kind = NodeState['kind']
export const KINDS: readonly Kind[] = ['asset', 'task', 'sensor']

export type LastRun = 'any' | 'hour' | 'day' | 'week_plus' | 'never'
export const LAST_RUNS: readonly LastRun[] = ['any', 'hour', 'day', 'week_plus', 'never']

export interface Filters {
  query: string
  /** Empty = any state. */
  states: Severity[]
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
  kinds: [],
  lastRun: 'any',
  scheduled: false,
  partitioned: false,
}

/** What the Tasks page opens on: most tasks always run, and only these need a look. */
export const NEEDS_ATTENTION: readonly Severity[] = ['failed', 'stale', 'partial', 'unsynced']

/**
 * With the default in force, a row that ran this recently is shown whatever its state: a task
 * that just ran is what its page is opened for, and tasks record no cache state to pick it by.
 */
export const RECENT_MS = 24 * 3_600_000

/** The default's states are in force: only `state=all` in the URL lifts them. */
export function onDefaultStates(f: Filters, defaults: readonly Severity[]): boolean {
  return defaults.length > 0 && f.states.length === defaults.length && defaults.every((s) => f.states.includes(s))
}

export function isFiltered(f: Filters): boolean {
  return (
    f.query.trim() !== '' ||
    f.states.length > 0 ||
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

type Group = 'states' | 'kinds' | 'lastRun' | 'scheduled' | 'partitioned'

export interface FilterOptions {
  /** Rows that ran within this many ms pass the state filter whatever their state. */
  recentMs?: number
}

/** Rows passing every filter except those of `skip` (for faceted counts). */
function filterExcept(rows: AssetRow[], f: Filters, nowMs: number, skip?: Group, opts: FilterOptions = {}): AssetRow[] {
  const recent = (r: AssetRow) =>
    opts.recentMs !== undefined && r.lastAtMs !== null && nowMs - r.lastAtMs <= opts.recentMs
  return filterRows(rows, f.query).filter(
    (r) =>
      (skip === 'states' || f.states.length === 0 || f.states.includes(r.severity) || recent(r)) &&
      (skip === 'kinds' || f.kinds.length === 0 || f.kinds.includes(r.kind)) &&
      (skip === 'lastRun' || matchesLastRun(r, f.lastRun, nowMs)) &&
      (skip === 'scheduled' || !f.scheduled || r.nextRunMs !== null) &&
      (skip === 'partitioned' || !f.partitioned || r.partitioned),
  )
}

export function applyFilters(rows: AssetRow[], f: Filters, nowMs: number, opts: FilterOptions = {}): AssetRow[] {
  return filterExcept(rows, f, nowMs, undefined, opts)
}

export interface FacetCounts {
  states: Record<Severity, number>
  kinds: Record<Kind, number>
  lastRun: Record<LastRun, number>
  scheduled: number
  partitioned: number
}

export function facetCounts(rows: AssetRow[], f: Filters, nowMs: number, opts: FilterOptions = {}): FacetCounts {
  const states = Object.fromEntries(SEVERITIES.map((s) => [s, 0])) as Record<Severity, number>
  for (const r of filterExcept(rows, f, nowMs, 'states', opts)) states[r.severity] += 1

  const kinds = { asset: 0, task: 0, sensor: 0 } as Record<Kind, number>
  for (const r of filterExcept(rows, f, nowMs, 'kinds', opts)) kinds[r.kind] += 1

  const forLastRun = filterExcept(rows, f, nowMs, 'lastRun', opts)
  const lastRun = Object.fromEntries(
    LAST_RUNS.map((l) => [l, forLastRun.filter((r) => matchesLastRun(r, l, nowMs)).length]),
  ) as Record<LastRun, number>

  return {
    states,
    kinds,
    lastRun,
    scheduled: filterExcept(rows, f, nowMs, 'scheduled', opts).filter((r) => r.nextRunMs !== null)
      .length,
    partitioned: filterExcept(rows, f, nowMs, 'partitioned', opts).filter((r) => r.partitioned).length,
  }
}

// ── URL ───────────────────────────────────────────────────────────────────────

const PARAMS = ['q', 'state', 'kind', 'ran', 'scheduled', 'partitioned'] as const

function list<T extends string>(raw: string | null, allowed: readonly T[]): T[] {
  if (!raw) return []
  return raw.split(',').filter((v): v is T => (allowed as readonly string[]).includes(v))
}

/** `defaults`: the states of a URL that names none (`state=all` asks for every state). */
export function parseFilters(p: URLSearchParams, defaults: readonly Severity[] = []): Filters {
  const ran = p.get('ran')
  return {
    query: p.get('q') ?? '',
    states: p.has('state') ? list(p.get('state'), SEVERITIES) : [...defaults],
    kinds: list(p.get('kind'), KINDS),
    lastRun: LAST_RUNS.find((l) => l === ran) ?? 'any',
    scheduled: p.get('scheduled') === '1',
    partitioned: p.get('partitioned') === '1',
  }
}

/** `params` with the filter keys replaced by `f` (other keys are kept). */
export function writeFilters(params: URLSearchParams, f: Filters, defaults: readonly Severity[] = []): URLSearchParams {
  const p = new URLSearchParams(params)
  for (const k of PARAMS) p.delete(k)
  if (f.query.trim()) p.set('q', f.query)
  if (!onDefaultStates(f, defaults)) {
    if (f.states.length) p.set('state', f.states.join(','))
    else if (defaults.length) p.set('state', 'all')
  }
  if (f.kinds.length) p.set('kind', f.kinds.join(','))
  if (f.lastRun !== 'any') p.set('ran', f.lastRun)
  if (f.scheduled) p.set('scheduled', '1')
  if (f.partitioned) p.set('partitioned', '1')
  return p
}
