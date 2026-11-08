/**
 * Asset table filters — pure, URL-backed.
 *
 * Every filter narrows the rows (they combine with AND; a multi-select group
 * matches any of its picks). Chip counts are faceted: each group's counts
 * apply every *other* active filter, so a count is always what clicking that
 * chip would show.
 */

import { match } from 'ts-pattern'
import { filterRows, type AssetRow } from './assetTable'
import { CACHE_STATES, type CacheState } from './cacheStateDisplay'
import type { NodeState } from './types'

export type Kind = NodeState['kind']
const KINDS: readonly Kind[] = ['asset', 'task', 'sensor']

export type LastRun = 'any' | 'hour' | 'day' | 'week_plus' | 'never'
export const LAST_RUNS: readonly LastRun[] = ['any', 'hour', 'day', 'week_plus', 'never']

export interface Filters {
  query: string
  /** Empty = any state. The CLI's words (`cache.state`). */
  states: CacheState[]
  /** Only nodes whose last attempt failed: a fact beside the state, so a filter of its own. */
  failed: boolean
  /** Only nodes a failure upstream keeps from running (`blocked`): derived, so a filter of its own. */
  blocked: boolean
  /** Only pinned nodes (`barca pin`): they read `pinned`, so a filter of its own. */
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
  failed: false,
  blocked: false,
  pinned: false,
  kinds: [],
  lastRun: 'any',
  scheduled: false,
  partitioned: false,
}

/** The state filters a page opens on, before the URL says otherwise. */
export type StateDefaults = Pick<Filters, 'states' | 'failed' | 'blocked'>

export const NO_DEFAULTS: StateDefaults = { states: [], failed: false, blocked: false }

/**
 * What the Tasks page opens on: a task always runs, so its state says nothing to pick it by; what
 * needs a look is a failed last attempt, and a state other than `always_runs` (blocked, stale
 * keys, an unsynced result).
 */
export const NEEDS_ATTENTION: StateDefaults = { states: ['blocked', 'stale', 'partial', 'unsynced'], failed: true, blocked: true }

/**
 * With the default in force, a row that ran this recently is shown whatever its state: a task
 * that just ran is what its page is opened for, and tasks record no cache state to pick it by.
 */
export const RECENT_MS = 24 * 3_600_000

const hasDefaults = (d: StateDefaults) => d.states.length > 0 || d.failed || d.blocked

/** The default's states are in force: only `state=all` in the URL lifts them. */
export function onDefaultStates(f: Filters, defaults: StateDefaults): boolean {
  return hasDefaults(defaults) && !f.pinned && f.failed === defaults.failed && f.blocked === defaults.blocked && f.states.length === defaults.states.length && defaults.states.every((s) => f.states.includes(s))
}

export function isFiltered(f: Filters): boolean {
  return (
    f.query.trim() !== '' ||
    f.states.length > 0 ||
    f.failed ||
    f.blocked ||
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

type Group = 'states' | 'kinds' | 'lastRun' | 'scheduled' | 'partitioned'

/**
 * Blocked, whichever way it is known: a failure upstream the client found (`blockedBy`), or the
 * server's `blocked` state (keys that wait on the same keys of a failed input). One word, one chip.
 */
function isBlocked(r: Pick<AssetRow, 'blockedBy' | 'state'>): boolean {
  return r.blockedBy !== null || r.state === 'blocked'
}

/**
 * The state filter, as a group, by the one state each row shows: `failing` (a failed last
 * attempt), `blocked` (a failure upstream keeps it from running) or else its cache state, so a chip's count is the rows it shows.
 */
function matchesStates(r: AssetRow, f: Filters): boolean {
  if (f.states.length === 0 && !f.failed && !f.blocked && !f.pinned) return true
  return r.lastFailed ? f.failed : r.pinned !== null ? f.pinned : isBlocked(r) ? f.blocked : f.states.includes(r.state)
}

interface FilterOptions {
  /** Rows that ran within this many ms pass the state filter whatever their state. */
  recentMs?: number
}

/** Rows passing every filter except those of `skip` (for faceted counts). */
function filterExcept(rows: AssetRow[], f: Filters, nowMs: number, skip?: Group, opts: FilterOptions = {}): AssetRow[] {
  const recent = (r: AssetRow) =>
    opts.recentMs !== undefined && r.lastAtMs !== null && nowMs - r.lastAtMs <= opts.recentMs
  return filterRows(rows, f.query).filter(
    (r) =>
      (skip === 'states' || matchesStates(r, f) || recent(r)) &&
      (skip === 'kinds' || f.kinds.length === 0 || f.kinds.includes(r.kind)) &&
      (skip === 'lastRun' || matchesLastRun(r, f.lastRun, nowMs)) &&
      (skip === 'scheduled' || !f.scheduled || r.nextRunMs !== null) &&
      (skip === 'partitioned' || !f.partitioned || r.partitioned),
  )
}

export function applyFilters(rows: AssetRow[], f: Filters, nowMs: number, opts: FilterOptions = {}): AssetRow[] {
  return filterExcept(rows, f, nowMs, undefined, opts)
}

interface FacetCounts {
  states: Record<CacheState, number>
  /** Rows whose last attempt failed. */
  failed: number
  /** Rows a failure upstream keeps from running. */
  blocked: number
  /** Pinned rows. */
  pinned: number
  kinds: Record<Kind, number>
  lastRun: Record<LastRun, number>
  scheduled: number
  partitioned: number
}

export function facetCounts(rows: AssetRow[], f: Filters, nowMs: number, opts: FilterOptions = {}): FacetCounts {
  const states = Object.fromEntries(CACHE_STATES.map((s) => [s, 0])) as Record<CacheState, number>
  let failed = 0
  let blocked = 0
  let pinned = 0
  for (const r of filterExcept(rows, f, nowMs, 'states', opts)) {
    if (r.lastFailed) failed += 1
    else if (r.pinned !== null) pinned += 1
    else if (isBlocked(r)) blocked += 1
    else states[r.state] += 1
  }

  const kinds = { asset: 0, task: 0, sensor: 0 } as Record<Kind, number>
  for (const r of filterExcept(rows, f, nowMs, 'kinds', opts)) kinds[r.kind] += 1

  const forLastRun = filterExcept(rows, f, nowMs, 'lastRun', opts)
  const lastRun = Object.fromEntries(
    LAST_RUNS.map((l) => [l, forLastRun.filter((r) => matchesLastRun(r, l, nowMs)).length]),
  ) as Record<LastRun, number>

  return {
    states,
    failed,
    blocked,
    pinned,
    kinds,
    lastRun,
    scheduled: filterExcept(rows, f, nowMs, 'scheduled', opts).filter((r) => r.nextRunMs !== null)
      .length,
    partitioned: filterExcept(rows, f, nowMs, 'partitioned', opts).filter((r) => r.partitioned).length,
  }
}

// ── URL ───────────────────────────────────────────────────────────────────────

const PARAMS = ['q', 'state', 'failed', 'blocked', 'pinned', 'kind', 'ran', 'scheduled', 'partitioned'] as const

function list<T extends string>(raw: string | null, allowed: readonly T[]): T[] {
  if (!raw) return []
  return raw.split(',').filter((v): v is T => (allowed as readonly string[]).includes(v))
}

/**
 * `defaults`: the state filters of a URL that names none (`state=all` asks for every state).
 * `state=` carries the CLI's words; `failed=1` asks for nodes whose last attempt failed, `blocked=1` for those a failure upstream keeps from running.
 */
export function parseFilters(p: URLSearchParams, defaults: StateDefaults = NO_DEFAULTS): Filters {
  const ran = p.get('ran')
  const named = p.has('state') || p.has('failed') || p.has('blocked') || p.has('pinned')
  return {
    query: p.get('q') ?? '',
    states: named ? list(p.get('state'), CACHE_STATES) : [...defaults.states],
    failed: named ? p.get('failed') === '1' : defaults.failed,
    blocked: named ? p.get('blocked') === '1' : defaults.blocked,
    pinned: named ? p.get('pinned') === '1' : false,
    kinds: list(p.get('kind'), KINDS),
    lastRun: LAST_RUNS.find((l) => l === ran) ?? 'any',
    scheduled: p.get('scheduled') === '1',
    partitioned: p.get('partitioned') === '1',
  }
}

/** `params` with the filter keys replaced by `f` (other keys are kept). */
export function writeFilters(params: URLSearchParams, f: Filters, defaults: StateDefaults = NO_DEFAULTS): URLSearchParams {
  const p = new URLSearchParams(params)
  for (const k of PARAMS) p.delete(k)
  if (f.query.trim()) p.set('q', f.query)
  if (!onDefaultStates(f, defaults)) {
    if (f.states.length) p.set('state', f.states.join(','))
    else if (hasDefaults(defaults)) p.set('state', 'all')
    if (f.failed) p.set('failed', '1')
    if (f.blocked) p.set('blocked', '1')
    if (f.pinned) p.set('pinned', '1')
  }
  if (f.kinds.length) p.set('kind', f.kinds.join(','))
  if (f.lastRun !== 'any') p.set('ran', f.lastRun)
  if (f.scheduled) p.set('scheduled', '1')
  if (f.partitioned) p.set('partitioned', '1')
  return p
}
