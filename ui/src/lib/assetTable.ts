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
import { formatAgo, parseTime } from './time'
import type { CacheStatus, NodeRunning, NodeState, StatusKind } from './types'

export { formatAgo } from './time'

/** How much a node needs attention, most urgent first. */
export type Severity =
  | 'failed'
  | 'stale'
  | 'never_run'
  | 'partial'
  | 'unknown'
  | 'always_runs'
  | 'unsynced'
  | 'cached'

export const SEVERITIES: readonly Severity[] = [
  'failed',
  'stale',
  'never_run',
  'partial',
  'unknown',
  'always_runs',
  'unsynced',
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
  /** Short label for the state column, e.g. `Stale · code changed`. */
  stateLabel: string
  /** Why, in words — the server's own explanation (tooltip). */
  stateHint: string
  last: { status: string; ago: string; error: string | null } | null
  /** When the latest attempt was recorded (ms since epoch). */
  lastAtMs: number | null
  /** When it last succeeded; null if it never did. */
  lastSuccessAt: string | null
  /** The first failed attempt after its last clean run, while it fails. */
  failingSince: string | null
  /** Runs in a row that failed it. */
  failures: number
  /** Median of recent successful runs, formatted; null if it never succeeded. */
  typical: string | null
  typicalSeconds: number | null
  p95: string | null
  /** Next cron fire time (ms since epoch), if scheduled. */
  nextRunMs: number | null
  /** The run working on the node now, whoever started it. */
  running: NodeRunning | null
}

/** A failed latest attempt outranks any cache state — it's the thing to look at. */
export function severityOf(node: NodeState): Severity {
  // A partitioned node's last materialization is one key: its failed keys are counted.
  if (node.last_materialization?.status === 'failed' || node.partitions?.failed) return 'failed'
  return match(node.cache.state)
    .with('cached', (): Severity => 'cached')
    .with('unsynced', (): Severity => 'unsynced')
    .with('stale', (): Severity => 'stale')
    .with('never_run', (): Severity => 'never_run')
    .with('partial', (): Severity => 'partial')
    // Keys waiting on failed upstream keys: as urgent as a stale node, and said in the label.
    .with('blocked', (): Severity => 'stale')
    .with('unknown', (): Severity => 'unknown')
    .with('always_runs', (): Severity => 'always_runs')
    .exhaustive()
}

/** `40 of 3,077 keys failed`, for a node with failed keys. */
export function failedKeysLabel(node: NodeState): string | null {
  const p = node.partitions
  return p?.failed ? `${p.failed.toLocaleString()} of ${p.total.toLocaleString()} keys failed` : null
}

/** A partitioned node's keys as parts of a whole. Parts add up to `total`. */
export function keyBreakdown(p: NonNullable<NodeState['partitions']>): { cached: number; failed: number; stale: number; total: number } {
  const failed = p.failed ?? 0
  return { cached: p.cached, failed, stale: Math.max(p.total - p.cached - failed, 0), total: p.total }
}

/** Severity → the design system's status vocabulary (colors). */
export function severityStatus(severity: Severity): StatusKind {
  return match(severity)
    .with('failed', (): StatusKind => 'failed')
    .with('stale', 'partial', 'unsynced', (): StatusKind => 'warning')
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
    .with('upstream_failed', () => 'upstream failed')
    .with('sensor_output_unknown', () => 'sensor')
    .with('cutoff_pending', () => 'cutoff')
    .with('failed', () => 'last run failed')
    .with('artifact_missing', () => 'artifact missing')
    .with('evicted', () => 'evicted')
    .with('local_only', () => 'local only')
    .with('store_only', () => 'store only')
    .with('mismatch', () => 'store copy differs')
    .with('not_in_ledger', () => 'not in ledger')
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
    .with('cached', () => 'Healthy')
    .with('unsynced', () => `Unsynced · ${staleReason(node.cache.reason)}`)
    .with('stale', () => `Stale · ${staleReason(node.cache.reason)}`)
    .with('never_run', () => 'Not run')
    .with('partial', () =>
      node.partitions
        ? `${node.partitions.missing.toLocaleString()} of ${node.partitions.total.toLocaleString()} keys stale`
        : 'Stale (some keys)',
    )
    .with('blocked', () =>
      node.partitions
        ? `Blocked · ${node.partitions.missing.toLocaleString()} keys wait on upstream`
        : 'Blocked · upstream failed',
    )
    .with('unknown', () => 'Unknown')
    .with('always_runs', () => 'Always runs')
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
        // One word; the reason, and whether a good result remains, is in the hint and the panel.
        stateLabel: severity === 'failed' ? failedKeysLabel(n) ?? 'Failed' : label,
        stateHint:
          severity === 'failed' ? `The latest attempt failed. ${n.cache.detail}` : n.cache.detail,
        last: last
          ? { status: n.partitions?.failed ? 'failed' : last.status, ago: formatAgo(last.created_at, nowMs), error: last.error ?? null }
          : null,
        lastAtMs: last ? parseTime(last.created_at) : null,
        lastSuccessAt: n.last_success_at,
        failingSince: n.failing_since,
        failures: n.consecutive_failures,
        // Recorded per key, whichever call computed it.
        typical: n.durations ? `${formatSeconds(n.durations.median_seconds)}${n.partitioned ? ' / key' : ''}` : null,
        typicalSeconds: n.durations?.median_seconds ?? null,
        p95: n.durations ? formatSeconds(n.durations.p95_seconds) : null,
        nextRunMs: n.next_run === null ? null : n.next_run * 1000,
        running: n.running ?? null,
      }
    })
    .sort((a, b) => RANK[a.severity] - RANK[b.severity] || a.name.localeCompare(b.name))
}

// ── Sorting ───────────────────────────────────────────────────────────────────

export type SortKey = 'name' | 'state' | 'last' | 'success' | 'typical' | 'next'
export type SortDir = 'asc' | 'desc'
export interface Sort {
  key: SortKey
  dir: SortDir
}

export const SORT_KEYS: readonly SortKey[] = ['name', 'state', 'last', 'success', 'typical', 'next']

/** What needs attention first. */
export const DEFAULT_SORT: Sort = { key: 'state', dir: 'asc' }

/** The direction a column sorts in on its first click: the useful end first. */
function firstDir(key: SortKey): SortDir {
  return match(key)
    .with('name', 'state', 'next', 'success', (): SortDir => 'asc') // A→Z, most urgent, soonest, latest
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
    .with('success', () => (row.lastSuccessAt === null ? null : parseTime(row.lastSuccessAt)))
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
