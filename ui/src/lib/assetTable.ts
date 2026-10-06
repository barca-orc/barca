/**
 * Asset table presentation logic — pure, exhaustively matched.
 *
 * Turns `GET /state` rows (`barca status` entries plus durations and next run)
 * into what the table shows: a severity per node (what needs attention sorts
 * first), a short state label with the server's explanation, the latest
 * attempt, typical durations and the next scheduled run. Returns plain
 * descriptors; `AssetsPage` only maps them to elements.
 */

import { match } from 'ts-pattern'
import type { CacheStatus, NodeState, StatusKind } from './types'

/** How much a node needs attention, most urgent first. */
export type Severity =
  | 'failed'
  | 'stale'
  | 'never_run'
  | 'partial'
  | 'unknown'
  | 'always_runs'
  | 'cached'

export const SEVERITIES: readonly Severity[] = [
  'failed',
  'stale',
  'never_run',
  'partial',
  'unknown',
  'always_runs',
  'cached',
]

export interface AssetRow {
  id: string
  /** Function name — what `get`, `run` and `--refresh` accept. */
  name: string
  /** Source file — everything before the last `:` of the id. */
  file: string
  kind: NodeState['kind']
  partitioned: boolean
  severity: Severity
  /** Short label for the state column, e.g. `stale · code changed`. */
  stateLabel: string
  /** Why, in words — the server's own explanation (tooltip). */
  stateHint: string
  last: { status: string; ago: string; error: string | null } | null
  /** When the latest attempt was recorded (ms since epoch). */
  lastAtMs: number | null
  /** Median of recent successful runs, formatted; null if it never succeeded. */
  typical: string | null
  typicalSeconds: number | null
  p95: string | null
  /** Next cron fire time (ms since epoch), if scheduled. */
  nextRunMs: number | null
}

/** A failed latest attempt outranks any cache state — it's the thing to look at. */
export function severityOf(node: NodeState): Severity {
  if (node.last_materialization?.status === 'failed') return 'failed'
  return match(node.cache.state)
    .with('cached', (): Severity => 'cached')
    .with('stale', (): Severity => 'stale')
    .with('never_run', (): Severity => 'never_run')
    .with('partial', (): Severity => 'partial')
    .with('unknown', (): Severity => 'unknown')
    .with('always_runs', (): Severity => 'always_runs')
    .exhaustive()
}

/** Severity → the design system's status vocabulary (colors). */
export function severityStatus(severity: Severity): StatusKind {
  return match(severity)
    .with('failed', (): StatusKind => 'failed')
    .with('stale', 'partial', (): StatusKind => 'warning')
    .with('never_run', (): StatusKind => 'queued')
    .with('unknown', 'always_runs', (): StatusKind => 'skipped')
    .with('cached', (): StatusKind => 'success')
    .exhaustive()
}

/** Why a stale node will re-run, in a word or two. */
function staleReason(reason: CacheStatus['reason']): string {
  return match(reason)
    .with('changed', () => 'code changed')
    .with('upstream_stale', () => 'upstream')
    .with('sensor_output_unknown', () => 'sensor')
    .with('failed', () => 'last run failed')
    .with(
      'materialized',
      'no_record',
      'partitions_missing',
      'partitions_unknown',
      'task',
      'sensor',
      () => reason.replace(/_/g, ' '),
    )
    .exhaustive()
}

function cacheLabel(node: NodeState): string {
  return match(node.cache.state)
    .with('cached', () => 'cached')
    .with('stale', () => `stale · ${staleReason(node.cache.reason)}`)
    .with('never_run', () => 'never run')
    .with('partial', () =>
      node.partitions ? `partial · ${node.partitions.cached}/${node.partitions.total}` : 'partial',
    )
    .with('unknown', () => 'unknown')
    .with('always_runs', () => 'always runs')
    .exhaustive()
}

function fileOf(id: string): string {
  const i = id.lastIndexOf(':')
  return i < 0 ? '' : id.slice(0, i)
}

const RANK: Record<Severity, number> = Object.fromEntries(
  SEVERITIES.map((s, i) => [s, i]),
) as Record<Severity, number>

/** One row per node, sorted by severity, then name. */
export function buildRows(nodes: NodeState[], nowMs: number): AssetRow[] {
  return nodes
    .map((n): AssetRow => {
      const severity = severityOf(n)
      const label = cacheLabel(n)
      const last = n.last_materialization
      return {
        id: n.id,
        name: n.name,
        file: fileOf(n.id),
        kind: n.kind,
        partitioned: n.partitioned,
        severity,
        stateLabel: severity === 'failed' ? `failed · ${label}` : label,
        stateHint:
          severity === 'failed' ? `The latest attempt failed. ${n.cache.detail}` : n.cache.detail,
        last: last
          ? { status: last.status, ago: formatAgo(last.created_at, nowMs), error: last.error ?? null }
          : null,
        lastAtMs: last ? parseUtc(last.created_at) : null,
        typical: n.durations ? formatSeconds(n.durations.median_seconds) : null,
        typicalSeconds: n.durations?.median_seconds ?? null,
        p95: n.durations ? formatSeconds(n.durations.p95_seconds) : null,
        nextRunMs: n.next_run === null ? null : n.next_run * 1000,
      }
    })
    .sort((a, b) => RANK[a.severity] - RANK[b.severity] || a.name.localeCompare(b.name))
}

// ── Sorting ───────────────────────────────────────────────────────────────────

export type SortKey = 'name' | 'state' | 'last' | 'typical' | 'next'
export type SortDir = 'asc' | 'desc'
export interface Sort {
  key: SortKey
  dir: SortDir
}

export const SORT_KEYS: readonly SortKey[] = ['name', 'state', 'last', 'typical', 'next']

/** What needs attention first. */
export const DEFAULT_SORT: Sort = { key: 'state', dir: 'asc' }

/** The direction a column sorts in on its first click: the useful end first. */
function firstDir(key: SortKey): SortDir {
  return match(key)
    .with('name', 'state', 'next', (): SortDir => 'asc') // A→Z, most urgent, soonest
    .with('last', 'typical', (): SortDir => 'desc') // most recent, slowest
    .exhaustive()
}

/** A header click: a new column starts at its useful end; the same one reverses. */
export function nextSort(current: Sort, key: SortKey): Sort {
  if (current.key !== key) return { key, dir: firstDir(key) }
  return { key, dir: current.dir === 'asc' ? 'desc' : 'asc' }
}

/** The sort from URL params; anything unrecognised falls back to the default. */
export function parseSort(key: string | null, dir: string | null): Sort {
  const k = SORT_KEYS.find((s) => s === key)
  if (!k) return DEFAULT_SORT
  return { key: k, dir: dir === 'asc' || dir === 'desc' ? dir : firstDir(k) }
}

function sortValue(row: AssetRow, key: SortKey): string | number | null {
  return match(key)
    .with('name', () => row.name)
    .with('state', () => RANK[row.severity])
    .with('last', () => row.lastAtMs)
    .with('typical', () => row.typicalSeconds)
    .with('next', () => row.nextRunMs)
    .exhaustive()
}

/**
 * Sort by one column. Rows without a value (never ran, never scheduled) go last
 * in both directions; ties fall back to severity, then name.
 */
export function sortRows(rows: AssetRow[], sort: Sort): AssetRow[] {
  const sign = sort.dir === 'asc' ? 1 : -1
  return [...rows].sort((a, b) => {
    const va = sortValue(a, sort.key)
    const vb = sortValue(b, sort.key)
    if (va === null || vb === null) {
      if (va !== vb) return va === null ? 1 : -1
    } else if (va !== vb) {
      const cmp = typeof va === 'string' ? va.localeCompare(vb as string) : va - (vb as number)
      return sign * cmp
    }
    return RANK[a.severity] - RANK[b.severity] || a.name.localeCompare(b.name)
  })
}

/** Case-insensitive substring match on name or full id; blank keeps all. */
export function filterRows(rows: AssetRow[], query: string): AssetRow[] {
  const q = query.trim().toLowerCase()
  if (!q) return rows
  return rows.filter((r) => r.name.toLowerCase().includes(q) || r.id.toLowerCase().includes(q))
}

/** Count of rows per severity (every severity present, zero or not). */
export function summarize(rows: AssetRow[]): Record<Severity, number> {
  const out = Object.fromEntries(SEVERITIES.map((s) => [s, 0])) as Record<Severity, number>
  for (const r of rows) out[r.severity] += 1
  return out
}

export function formatSeconds(s: number): string {
  if (s < 60) return `${s.toFixed(1)}s`
  if (s < 3600) return `${(s / 60).toFixed(1)}m`
  return `${(s / 3600).toFixed(1)}h`
}

/** `created_at` is UTC `YYYY-MM-DD HH:MM:SS` (SQLite `datetime('now')`). */
function parseUtc(createdAt: string): number | null {
  const t = Date.parse(`${createdAt.replace(' ', 'T')}Z`)
  return Number.isNaN(t) ? null : t
}

export function formatAgo(createdAt: string, nowMs: number): string {
  const t = parseUtc(createdAt)
  if (t === null) return createdAt
  const s = Math.max(0, (nowMs - t) / 1000)
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)}m ago`
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`
  return `${Math.floor(s / 86400)}d ago`
}
