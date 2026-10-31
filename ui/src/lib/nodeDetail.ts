/**
 * Node detail panel — pure helpers over data the server already returns
 * (`GET /state` for the node and its neighbours, `GET /assets/{id}` for its
 * run history). Nothing here fetches.
 */

import { formatSeconds } from './assetTable'
import type { AssetRunEntry, CacheStatus } from './types'
import type { RunNodeSummary } from './generated/RunNodeSummary'

/** Nodes that take `id` as an input, sorted by name. */
export function downstreamOf<T extends { id: string; inputs: string[] }>(id: string, nodes: readonly T[]): T[] {
  const name = (n: T) => n.id.slice(n.id.lastIndexOf(':') + 1)
  return nodes
    .filter((n) => n.inputs.includes(id))
    .sort((a, b) => name(a).localeCompare(name(b)))
}

export interface HistoryBar {
  createdAt: string
  /** Height relative to the slowest run shown (failures with no duration: full). */
  heightPct: number
  failed: boolean
  /** Tooltip text. */
  label: string
}

/** Bars for the recent-runs chart, oldest first (the API returns newest first). */
export function historyBars(runs: AssetRunEntry[]): HistoryBar[] {
  const max = Math.max(0, ...runs.map((r) => r.elapsed_seconds ?? 0))
  return [...runs].reverse().map((r) => {
    const failed = r.status !== 'success'
    const e = r.elapsed_seconds
    return {
      createdAt: r.created_at,
      heightPct: e === null || max === 0 ? 100 : Math.max(4, Math.round((e / max) * 100)),
      failed,
      label: e === null ? r.status : `${r.status} · ${formatSeconds(e)}`,
    }
  })
}

export interface DurationBin {
  /** Lower edge, seconds (inclusive). */
  from: number
  /** Upper edge, seconds (exclusive, except the last bin). */
  to: number
  count: number
  /** Height relative to the fullest bin, 0-100. */
  heightPct: number
  /** True when the last attempt's duration falls in this bin. */
  isLast: boolean
  label: string
}

/**
 * Histogram of successful-run durations (`binCount` equal-width bins from the
 * fastest to the slowest run). `lastSeconds` marks the bin the last attempt
 * landed in. Returns [] with fewer than two timed runs: a distribution of one
 * point says nothing the number doesn't.
 */
export function durationHistogram(
  runs: AssetRunEntry[],
  lastSeconds: number | null,
  binCount = 10,
): DurationBin[] {
  const xs = runs
    .filter((r) => r.status === 'success' && r.elapsed_seconds !== null)
    .map((r) => r.elapsed_seconds as number)
  if (xs.length < 2) return []
  const lo = Math.min(...xs)
  const hi = Math.max(...xs)
  // All runs identical: a single bin.
  const n = hi === lo ? 1 : binCount
  const width = (hi - lo) / n
  const index = (x: number) => (n === 1 ? 0 : Math.min(n - 1, Math.floor((x - lo) / width)))
  const counts = new Array<number>(n).fill(0)
  for (const x of xs) counts[index(x)] = (counts[index(x)] ?? 0) + 1
  const peak = Math.max(...counts)
  const lastBin = lastSeconds !== null && lastSeconds >= lo && lastSeconds <= hi ? index(lastSeconds) : -1
  return counts.map((count, i) => {
    const from = lo + i * width
    const to = n === 1 ? hi : lo + (i + 1) * width
    return {
      from,
      to,
      count,
      heightPct: count === 0 ? 0 : Math.max(6, Math.round((count / peak) * 100)),
      isLast: i === lastBin,
      label: `${formatSeconds(from)}–${formatSeconds(to)} · ${count} run${count === 1 ? '' : 's'}`,
    }
  })
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i += 1
  }
  return `${v.toFixed(1)} ${units[i]}`
}

/** The first 12 hex digits of a run hash — enough to tell versions apart. */
export function shortHash(h: string): string {
  return h.slice(0, 12)
}

type Cause = NonNullable<CacheStatus['change_causes']>[number]

const CAUSE: Record<Cause, string> = {
  code_changed: 'code',
  inputs_changed: 'input',
  env_changed: 'env',
  upstream_stale: 'upstream',
}

/**
 * What the run hash says moved, in a line: `Failed after a code change`, `Same code, new input`.
 * Null when the server recorded no cause: older materializations carry none, and this never guesses.
 */
export function whatChanged({ change_causes: causes = [] }: Pick<CacheStatus, 'change_causes'>, failed: boolean): string | null {
  const moved = causes.filter((c) => c !== 'upstream_stale')
  if (moved.length === 0) return causes.length ? 'Waiting on a stale upstream' : null
  if (moved.length === 1 && moved[0] === 'inputs_changed' && causes.includes('upstream_stale') && !failed) return 'Waiting on a stale upstream'
  if (moved.length === 1 && moved[0] === 'inputs_changed') return failed ? 'Failed with the same code on new input' : 'Same code, new input'
  const noun = `${moved.map((c) => CAUSE[c]).join(' and ')} change${moved.length > 1 ? 's' : ''}`
  if (!failed) return noun[0]!.toUpperCase() + noun.slice(1)
  if (moved.length > 1) return `Failed after ${noun}`
  return `Failed after ${moved[0] === 'env_changed' ? 'an' : 'a'} ${noun}`
}

/** localStorage key: the path prefixes an editor on this machine can open, one per line or comma. */
export const EDITOR_ROOTS_KEY = 'barca.editor-roots'

/** The configured editor roots: absolute path prefixes, blank lines dropped. */
export function editorRoots(stored: string | null | undefined): string[] {
  return (stored ?? '').split(/[\n,]/).map((s) => s.trim()).filter(Boolean)
}

/**
 * Whether an editor link makes sense: the server's project root is under a configured prefix,
 * so the path is one this browser's machine has. The server's own path is useless elsewhere.
 */
export function editorOpens(root: string | null | undefined, roots: readonly string[]): boolean {
  if (!root) return false
  return roots.some((prefix) => root === prefix.replace(/\/+$/, '') || root.startsWith(prefix.endsWith('/') ? prefix : `${prefix}/`))
}

/** A definition's file relative to the project root (`planning.py`, not `/srv/app/planning.py`). */
export function sourcePath(root: string | null | undefined, file: string): string {
  if (root && file.startsWith(`${root.replace(/\/+$/, '')}/`)) return file.slice(root.replace(/\/+$/, '').length + 1)
  return file
}

/** `vscode://file/…:12` for a node's definition; null when the server names no project root. */
export function editorLink(root: string | null | undefined, file: string, line: number): string | null {
  if (!root && !file.startsWith('/')) return null
  const path = file.startsWith('/') ? file : `${root}/${file}`
  return `vscode://file/${path.split('/').filter(Boolean).map(encodeURIComponent).join('/')}:${line}`
}

/**
 * `--partition` for one key as `GET /assets/{id}/partitions` lists it: `region=emea` as is, a
 * compound key (`region=emea,tier=key`) as the JSON object the flag takes.
 */
export function partitionSelection(key: string): string {
  const pairs = key.split(',')
  if (pairs.length === 1) return key
  const entries = pairs.map((pair) => {
    const i = pair.indexOf('=')
    return i < 0 ? null : [pair.slice(0, i), pair.slice(i + 1)]
  })
  if (entries.some((e) => e === null)) return key
  return JSON.stringify(Object.fromEntries(entries as [string, string][]))
}

/**
 * How fast a partitioned node's last run went, per unit of work: `0.4s / call · 5 calls` for
 * batched calls, `2.2s / run · 3,077 keys` when each key ran on its own. Null when it recorded
 * no time (every key failed).
 */
export function runPace(run: Pick<RunNodeSummary, 'calls' | 'keys' | 'elapsed_seconds'> | null | undefined): string | null {
  if (!run || run.keys === 0 || (run.elapsed_seconds === 0 && run.calls === 0)) return null
  if (run.calls > 0) return `${formatSeconds(run.elapsed_seconds / run.calls)} / call · ${run.calls.toLocaleString()} ${run.calls === 1 ? 'call' : 'calls'}`
  return `${formatSeconds(run.elapsed_seconds)} / run · ${run.keys.toLocaleString()} keys`
}

export interface BarSegments {
  cached: number
  failed: number
  stale: number
}

/**
 * The widths (percent) of a key bar's parts. A part that exists is at least `minPct` wide, taken
 * from the widest part: 7 failed keys of 3,077 are 0.2%, which a 300px bar never shows.
 */
export function barSegments({ cached, failed, stale, total }: { cached: number; failed: number; stale: number; total: number }, minPct = 2): BarSegments {
  if (total <= 0) return { cached: 0, failed: 0, stale: 0 }
  const out: BarSegments = { cached: (cached / total) * 100, failed: (failed / total) * 100, stale: (stale / total) * 100 }
  const keys = ['cached', 'failed', 'stale'] as const
  for (const k of keys) {
    if (out[k] > 0 && out[k] < minPct) {
      const widest = keys.reduce((a, b) => (out[a] >= out[b] ? a : b))
      if (widest !== k) out[widest] -= minPct - out[k]
      out[k] = minPct
    }
  }
  return out
}

/** A preview cell past `max` characters, cut for the table; the whole value opens on click. */
export function truncateCell(value: string, max = 80): { short: string; truncated: boolean } {
  if (value.length <= max) return { short: value, truncated: false }
  return { short: `${value.slice(0, max - 1)}…`, truncated: true }
}

/** A shape's `sample` as a table: rows of objects, one object (a dict's first keys), or plain values. */
export function previewRows(sample: unknown): { columns: string[]; rows: string[][] } | null {
  const isObject = typeof sample === 'object' && sample !== null && !Array.isArray(sample)
  if (isObject) return previewRows([sample])
  if (!Array.isArray(sample) || sample.length === 0) return null
  const cell = (v: unknown) => (typeof v === 'string' ? v : (JSON.stringify(v) ?? ''))
  const isRow = (r: unknown): r is Record<string, unknown> => typeof r === 'object' && r !== null && !Array.isArray(r)
  if (!sample.every(isRow)) return { columns: ['value'], rows: sample.map((v) => [cell(v)]) }
  const columns = [...new Set(sample.flatMap((r) => Object.keys(r)))]
  return { columns, rows: sample.map((r) => columns.map((c) => cell(r[c]))) }
}
