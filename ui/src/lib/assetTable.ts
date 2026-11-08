/**
 * Asset table presentation logic — pure.
 *
 * Turns `GET /state` rows (`barca status` entries plus durations and next run) into what the
 * table shows: the node's cache state in the CLI's word with the server's reason and detail,
 * whether its last attempt failed (a fact beside the state), the latest attempt, typical
 * durations and the next scheduled run. Returns plain descriptors; `AssetsPage` only maps them
 * to elements.
 */

import { match } from 'ts-pattern'
import { CACHE_STATES, cacheStateRank, lastAttemptFailed, type CacheState } from './cacheStateDisplay'
import { formatAgo, parseTime } from './time'
import type { CacheStatus, NodeRunning, NodeState } from './types'

export { formatAgo } from './time'

export interface AssetRow {
  id: string
  /** Function name — what `get`, `run` and `--refresh` accept. */
  name: string
  /** Source file — everything before the last `:` of the id. */
  file: string
  kind: NodeState['kind']
  partitioned: boolean
  /** `cache.state`, as the server says it. */
  state: CacheState
  /** `cache.reason`, verbatim. */
  reason: CacheStatus['reason']
  /** The latest attempt failed (or some keys did): shown beside the state, never in its place. */
  lastFailed: boolean
  /** The failed node upstream that keeps it from running (`blocked`); null when none. */
  blockedBy: string | null
  /** Pinned (`barca pin`): what it is pinned to, in a line; null when not pinned. Reads `pinned`. */
  pinned: string | null
  /** Why, in words — the server's own explanation (`cache.detail`, a tooltip). */
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
  /** Median of recent successful runs, formatted (`~2.3s / run` over a partitioned node's keys); null if it never succeeded. */
  typical: string | null
  typicalSeconds: number | null
  p95: string | null
  /** Next cron fire time (ms since epoch), if scheduled. */
  nextRunMs: number | null
  /** The run working on the node now, whoever started it. */
  running: NodeRunning | null
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

/**
 * A node's typical time, in seconds. Durations are recorded per key, whichever call computed
 * it, so a batched node's per-key median is a few milliseconds and says nothing; a partitioned
 * node's figure is the time of a run over all its keys, estimated from that median.
 */
export function typicalSeconds(n: Pick<NodeState, 'durations' | 'partitioned' | 'partitions'>): number | null {
  if (!n.durations) return null
  if (n.partitioned && n.partitions) return n.durations.median_seconds * n.partitions.total
  return n.durations.median_seconds
}

/** `3.0s`, or `~2.3s / run` for a partitioned node (its keys' medians summed, see `typicalSeconds`). */
export function typicalLabel(n: Pick<NodeState, 'durations' | 'partitioned' | 'partitions'>): string | null {
  const s = typicalSeconds(n)
  if (s === null) return null
  return n.partitioned && n.partitions ? `~${formatSeconds(s)} / run` : formatSeconds(s)
}

function fileOf(id: string): string {
  const i = id.lastIndexOf(':')
  return i < 0 ? '' : id.slice(0, i)
}

/**
 * What needs attention first: a failed last attempt, then the state's rank, then the name.
 * Presentation order only; the state itself is never changed by the attempt.
 */
function compareAttention(a: Pick<AssetRow, 'state' | 'lastFailed' | 'blockedBy' | 'pinned' | 'name'>, b: Pick<AssetRow, 'state' | 'lastFailed' | 'blockedBy' | 'pinned' | 'name'>): number {
  return Number(b.lastFailed) - Number(a.lastFailed) || Number(!b.lastFailed && b.pinned !== null) - Number(!a.lastFailed && a.pinned !== null) || Number(!!b.blockedBy) - Number(!!a.blockedBy) || cacheStateRank(a.state) - cacheStateRank(b.state) || a.name.localeCompare(b.name)
}

/** One row per node, what needs attention first. */
export function buildRows(nodes: NodeState[], nowMs: number, blocked: ReadonlyMap<string, string> = new Map(), pins: ReadonlyMap<string, string> = new Map()): AssetRow[] {
  return nodes
    .map((n): AssetRow => {
      const last = n.last_materialization
      return {
        id: n.id,
        name: n.name,
        file: fileOf(n.id),
        kind: n.kind,
        partitioned: n.partitioned,
        state: n.cache.state,
        reason: n.cache.reason,
        lastFailed: lastAttemptFailed(n),
        blockedBy: blocked.get(n.id) ?? null,
        pinned: pins.get(n.id) ?? null,
        stateHint: n.cache.detail,
        last: last
          ? { status: n.partitions?.failed ? 'failed' : last.status, ago: formatAgo(last.created_at, nowMs), error: last.error ?? null }
          : null,
        lastAtMs: last ? parseTime(last.created_at) : null,
        lastSuccessAt: n.last_success_at,
        failingSince: n.failing_since,
        failures: n.consecutive_failures,
        typical: typicalLabel(n),
        typicalSeconds: typicalSeconds(n),
        p95: n.durations ? formatSeconds(n.durations.p95_seconds) : null,
        nextRunMs: n.next_run === null ? null : n.next_run * 1000,
        running: n.running ?? null,
      }
    })
    .sort(compareAttention)
}

// ── Sorting ───────────────────────────────────────────────────────────────────

export type SortKey = 'name' | 'state' | 'last' | 'success' | 'typical' | 'next'
type SortDir = 'asc' | 'desc'
export interface Sort {
  key: SortKey
  dir: SortDir
}

const SORT_KEYS: readonly SortKey[] = ['name', 'state', 'last', 'success', 'typical', 'next']

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
    .with('state', () => (row.lastFailed ? 0 : row.pinned !== null ? 1 : row.blockedBy ? 2 : 3) * CACHE_STATES.length + cacheStateRank(row.state))
    .with('last', () => row.lastAtMs)
    .with('success', () => (row.lastSuccessAt === null ? null : parseTime(row.lastSuccessAt)))
    .with('typical', () => row.typicalSeconds)
    .with('next', () => row.nextRunMs)
    .exhaustive()
}

/**
 * Sort by one column. Rows without a value (never ran, never scheduled) go last
 * in both directions; ties fall back to what needs attention, then name.
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
    return compareAttention(a, b)
  })
}

/** Case-insensitive substring match on name or full id; blank keeps all. */
export function filterRows(rows: AssetRow[], query: string): AssetRow[] {
  const q = query.trim().toLowerCase()
  if (!q) return rows
  return rows.filter((r) => r.name.toLowerCase().includes(q) || r.id.toLowerCase().includes(q))
}

/** Count of rows per state (every state present, zero or not), and how many last attempts failed. */
export function summarize(rows: AssetRow[]): { states: Record<CacheState, number>; lastFailed: number } {
  const states = Object.fromEntries(CACHE_STATES.map((s) => [s, 0])) as Record<CacheState, number>
  let lastFailed = 0
  for (const r of rows) {
    states[r.state] += 1
    if (r.lastFailed) lastFailed += 1
  }
  return { states, lastFailed }
}

export function formatSeconds(s: number): string {
  if (s < 60) return `${s.toFixed(1)}s`
  if (s < 3600) return `${(s / 60).toFixed(1)}m`
  return `${(s / 3600).toFixed(1)}h`
}

// ── One line a row ────────────────────────────────────────────────────────────

/**
 * The Last run cell: one short line, no glyph (the state column has it), the time and, after a
 * failed attempt, when it last went well. `17m ago`, `failed 17m ago · last ok 2d ago`,
 * `failed 17m ago · never succeeded`, `not run`.
 */
export function lastRunText(row: Pick<AssetRow, 'last' | 'lastFailed' | 'lastSuccessAt'>, nowMs: number): string {
  if (!row.last) return 'not run'
  if (row.lastFailed || row.last.status === 'failed') {
    return `failed ${row.last.ago} · ${row.lastSuccessAt ? `last ok ${formatAgo(row.lastSuccessAt, nowMs)}` : 'never succeeded'}`
  }
  return row.last.status === 'success' ? row.last.ago : `${row.last.status} ${row.last.ago}`
}

/** The Last run cell's tooltip: the rest of the story while it fails, how long and how often. */
export function lastRunTitle(row: Pick<AssetRow, 'last' | 'lastFailed' | 'lastSuccessAt' | 'failingSince' | 'failures'>, nowMs: number): string {
  const line = lastRunText(row, nowMs)
  if (!row.failingSince) return line
  return `${line} · failing since ${formatAgo(row.failingSince, nowMs)} · ${row.failures} ${row.failures === 1 ? 'run' : 'runs'}`
}

/** Below this a typical duration says nothing a reader can use, and reads as a dash. */
const TYPICAL_MIN_SECONDS = 0.1

/** The Typical cell: the median, or a dash when there is none or it is under 0.1s. */
export function typicalText(row: Pick<AssetRow, 'typical' | 'typicalSeconds'>): string {
  return row.typical !== null && row.typicalSeconds !== null && row.typicalSeconds >= TYPICAL_MIN_SECONDS ? row.typical : '–'
}

/**
 * The optional columns, shown only when a row has something to say in them: Typical when some
 * node takes 0.1s or more, Next run when some node has a schedule. Judged over the page's rows,
 * not the filtered ones, so a filter never moves the columns.
 */
export function assetColumns(rows: readonly Pick<AssetRow, 'typicalSeconds' | 'nextRunMs'>[]): { typical: boolean; next: boolean } {
  return {
    typical: rows.some((r) => r.typicalSeconds !== null && r.typicalSeconds >= TYPICAL_MIN_SECONDS),
    next: rows.some((r) => r.nextRunMs !== null),
  }
}

/** A source file's rows, in the table's order, and the row that most needs attention. */
export interface FileGroup {
  file: string
  rows: AssetRow[]
  worst: AssetRow
}

/** The rows by file, each file where its first row falls in the sorted table. */
export function groupByFile(rows: readonly AssetRow[]): FileGroup[] {
  const groups = new Map<string, AssetRow[]>()
  for (const r of rows) {
    const list = groups.get(r.file)
    if (list) list.push(r)
    else groups.set(r.file, [r])
  }
  return [...groups].map(([file, list]) => ({
    file,
    rows: list,
    worst: list.reduce((a, b) => (compareAttention(b, a) < 0 ? b : a)),
  }))
}

/** One line of the table: a file's header, or a node's row. */
export type TableLine =
  | { kind: 'file'; group: FileGroup; collapsed: boolean }
  | { kind: 'row'; row: AssetRow }

/**
 * The table's lines: grouped, a header per file and its rows unless it is folded; else the rows.
 */
export function tableLines(rows: readonly AssetRow[], grouped: boolean, collapsed: ReadonlySet<string>): TableLine[] {
  if (!grouped) return rows.map((row) => ({ kind: 'row', row }))
  return groupByFile(rows).flatMap((group): TableLine[] => {
    const folded = collapsed.has(group.file)
    return [{ kind: 'file', group, collapsed: folded }, ...(folded ? [] : group.rows.map((row): TableLine => ({ kind: 'row', row })))]
  })
}
