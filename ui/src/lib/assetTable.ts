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
  severity: Severity
  /** Short label for the state column, e.g. `stale · code changed`. */
  stateLabel: string
  /** Why, in words — the server's own explanation (tooltip). */
  stateHint: string
  last: { status: string; ago: string; error: string | null } | null
  /** Median of recent successful runs, formatted; null if it never succeeded. */
  typical: string | null
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
        severity,
        stateLabel: severity === 'failed' ? `failed · ${label}` : label,
        stateHint:
          severity === 'failed' ? `The latest attempt failed. ${n.cache.detail}` : n.cache.detail,
        last: last
          ? { status: last.status, ago: formatAgo(last.created_at, nowMs), error: last.error ?? null }
          : null,
        typical: n.durations ? formatSeconds(n.durations.median_seconds) : null,
        p95: n.durations ? formatSeconds(n.durations.p95_seconds) : null,
        nextRunMs: n.next_run === null ? null : n.next_run * 1000,
      }
    })
    .sort((a, b) => RANK[a.severity] - RANK[b.severity] || a.name.localeCompare(b.name))
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
export function formatAgo(createdAt: string, nowMs: number): string {
  const t = Date.parse(`${createdAt.replace(' ', 'T')}Z`)
  if (Number.isNaN(t)) return createdAt
  const s = Math.max(0, (nowMs - t) / 1000)
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)}m ago`
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`
  return `${Math.floor(s / 86400)}d ago`
}
