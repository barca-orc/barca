/**
 * Node detail panel — pure helpers over data the server already returns
 * (`GET /state` for the node and its neighbours, `GET /assets/{id}` for its
 * run history). Nothing here fetches.
 */

import { match } from 'ts-pattern'
import { formatSeconds } from './assetTable'
import { plural } from './plural'
import type { AssetRunEntry, CacheStatus, FailureFacts, NodeState, PartitionState } from './types'
import type { LastRun } from './generated/LastRun'
import type { RunNodeSummary } from './generated/RunNodeSummary'

/** Nodes that take `id` as an input, sorted by name. */
export function downstreamOf<T extends { id: string; inputs: string[] }>(id: string, nodes: readonly T[]): T[] {
  const name = (n: T) => n.id.slice(n.id.lastIndexOf(':') + 1)
  return nodes
    .filter((n) => n.inputs.includes(id))
    .sort((a, b) => name(a).localeCompare(name(b)))
}

interface HistoryBar {
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

interface DurationBin {
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
export function sourcePath(root: string | null | undefined, file: string | null | undefined): string {
  if (!file) return ''
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
  return `${formatSeconds(run.elapsed_seconds)} / run · ${plural(run.keys, 'key')}`
}

interface BarSegments {
  cached: number
  failed: number
  blocked: number
  behind: number
}

/** A partitioned node's keys as its row counts them (`partitions`): the bar's parts, none worked out here. */
export interface KeyParts {
  cached: number
  failed: number
  /** Keys that wait on the failed keys of an input. */
  blocked: number
  /** Keys that are stale or never ran: a run computes them. */
  behind: number
  total: number
}

/** The parts of a key bar, read off a row's `partitions`. */
export function keyParts(p: Pick<PartitionState, 'total' | 'cached' | 'failed' | 'blocked' | 'behind'>): KeyParts {
  return { cached: p.cached, failed: p.failed ?? 0, blocked: p.blocked ?? 0, behind: p.behind, total: p.total }
}

/**
 * The widths (percent) of a key bar's parts. A part that exists is at least `minPct` wide, taken
 * from the widest part: 7 failed keys of 3,077 are 0.2%, which a 300px bar never shows.
 */
export function barSegments({ cached, failed, blocked, behind, total }: KeyParts, minPct = 2): BarSegments {
  if (total <= 0) return { cached: 0, failed: 0, blocked: 0, behind: 0 }
  const out: BarSegments = { cached: (cached / total) * 100, failed: (failed / total) * 100, blocked: (blocked / total) * 100, behind: (behind / total) * 100 }
  const keys = ['cached', 'failed', 'blocked', 'behind'] as const
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

/** Whether every recorded run failed: the History has no successful run to summarize. */
export function onlyFailures(runs: readonly AssetRunEntry[]): boolean {
  return runs.length > 0 && runs.every((r) => r.status !== 'success')
}

/** `assets.py:136` (`in broken_report` in the title): where the project's own code raised. */
export function raisedAtLabel(failure: FailureFacts | null | undefined): { text: string; full: string } | null {
  const at = failure?.raised_at
  if (!at) return null
  const base = at.file.split('/').pop() ?? at.file
  return { text: `${base}:${at.line}`, full: `${at.file}:${at.line}${at.function ? ` in ${at.function}` : ''}` }
}

/**
 * What differs between the failed attempt and the node's last success, from the run hashes
 * (barca is run-hash addressed): never a guess. `at` is the last success's time, when there is one.
 */
export function sinceLastSuccess(
  failure: FailureFacts | null | undefined,
  cache: Pick<CacheStatus, 'run_hash' | 'changed'>,
): { text: string; at: string | null } | null {
  if (!failure) return null
  const last = failure.last_success
  if (!last) return { text: 'Never succeeded: there is no earlier result to compare with', at: null }
  if (failure.run_hash && last.run_hash === failure.run_hash) {
    return { text: 'Same code and inputs as the last success (same run hash): the cause is outside the definition', at: last.created_at }
  }
  // `cache.changed` compares the current definition with the last success: it describes the
  // failed attempt only while the current run hash is still the one that failed.
  if (!failure.run_hash || cache.run_hash !== failure.run_hash) {
    return { text: 'Code or inputs changed after this attempt; run it again to compare with the last success', at: last.created_at }
  }
  if (cache.changed && cache.changed.length > 0) {
    const shown = cache.changed.slice(0, 3).join(', ')
    const more = cache.changed.length > 3 ? ` and ${cache.changed.length - 3} more` : ''
    return { text: `Changed since the last success: ${shown}${more}`, at: last.created_at }
  }
  return { text: 'Run hash differs from the last success; which part changed was not recorded', at: last.created_at }
}

/** What a `changed` entry is about: its first words (`helper`, `input`, `decorator argument`). */
const CHANGE_KINDS: ReadonlyArray<{ prefix: string; noun: string; counted: boolean }> = [
  { prefix: 'code', noun: 'code', counted: false },
  { prefix: 'kind', noun: 'kind', counted: false },
  { prefix: 'decorator argument ', noun: 'decorator argument', counted: true },
  { prefix: 'decorator ', noun: 'decorator', counted: true },
  { prefix: 'helper ', noun: 'helper', counted: true },
  { prefix: 'input ', noun: 'input', counted: true },
  { prefix: 'sensor ', noun: 'sensor', counted: true },
  { prefix: 'env ', noun: 'env var', counted: true },
  { prefix: 'value ', noun: 'key value', counted: true },
]

/**
 * `changed` (what moved the run hash) in a few words: `code, 2 inputs, 15 helpers`, in the order
 * the kinds first appear. An entry of no known kind is named as it is.
 */
export function changedSummary(changed: readonly string[]): string {
  const counts = new Map<string, { noun: string; counted: boolean; n: number }>()
  for (const entry of changed) {
    const kind = CHANGE_KINDS.find((k) => (k.counted ? entry.startsWith(k.prefix) : entry === k.prefix))
    const key = kind?.noun ?? entry
    const seen = counts.get(key)
    if (seen) seen.n += 1
    else counts.set(key, { noun: kind?.noun ?? entry, counted: kind?.counted ?? false, n: 1 })
  }
  return [...counts.values()].map((c) => (c.counted ? plural(c.n, c.noun) : c.noun)).join(', ')
}

/** Consecutive runs of the History that failed the same way, as one line. */
export interface RunGroup {
  /** The newest of them. */
  run: AssetRunEntry
  count: number
  /** When the oldest of them was recorded. */
  oldestAt: string
}

/** An error's first line, to tell whether two runs failed the same way. */
export function errorFirstLine(error: string | null | undefined): string {
  return (error ?? '').trim().split('\n')[0]!.trim()
}

/**
 * `runs` (newest first) with each stretch of identical failures folded into one entry: the same
 * status, the same error's first line and, for a partitioned node, the same key counts. A
 * schedule that fails every minute reads as one line, `×12`, not twelve. Successes stay apart.
 */
export function collapseRuns(runs: readonly AssetRunEntry[]): RunGroup[] {
  const out: RunGroup[] = []
  for (const run of runs) {
    const last = out.at(-1)
    const same = last && run.status !== 'success' && last.run.status === run.status
      && errorFirstLine(last.run.error_message) === errorFirstLine(run.error_message)
      && (last.run.keys ?? null) === (run.keys ?? null) && (last.run.failed_keys ?? null) === (run.failed_keys ?? null)
    if (last && same) {
      last.count += 1
      last.oldestAt = run.created_at
    } else out.push({ run, count: 1, oldestAt: run.created_at })
  }
  return out
}

/** `158 runs recorded`: a node's total in its history, never to be read as a failing streak. */
export function runsRecorded(total: number): string {
  return `${total.toLocaleString()} ${total === 1 ? 'run' : 'runs'} recorded`
}

/**
 * Why the History has no durations, when every run it lists failed. The list is the newest runs
 * only, so it claims "all" only when the failing streak covers every run recorded; otherwise it
 * names the streak, as the list and the failures view do.
 */
export function failuresOnlyNote(total: number, streak: number, listed: number, partitioned: boolean): string {
  if (streak >= total) {
    return partitioned
      ? `No run has computed every key yet (${total === 1 ? 'its only run' : `all ${total.toLocaleString()} runs`} had keys fail), so there are no durations to show.`
      : `No successful run yet: ${total === 1 ? 'its only attempt' : `all ${total.toLocaleString()} attempts`} failed, so there are no durations to show.`
  }
  const n = Math.max(streak, listed)
  return partitioned
    ? `Keys failed in each of the last ${n.toLocaleString()} runs (${runsRecorded(total)}), so there are no recent durations to show.`
    : `The last ${n.toLocaleString()} runs failed (${runsRecorded(total)}), so there are no recent durations to show.`
}

/**
 * A partitioned node's keys at a glance, the first thing its panel says: how many there are, how
 * many have a cached result, how many of those wrote no rows (when barca counted rows), and what
 * its newest run did to them. The newest run's figures are left out until a run recorded keys.
 */
export type KeyFigure = { id: 'total' | 'cached' | 'empty' | 'rebuilt' | 'failed'; label: string; value: number }

export function keyFigures(
  partitions: Pick<PartitionState, 'total' | 'cached' | 'empty'>,
  lastRun: Pick<LastRun, 'keys' | 'failed'> | null | undefined,
): KeyFigure[] {
  const figures: KeyFigure[] = [
    { id: 'total', label: 'keys', value: partitions.total },
    { id: 'cached', label: 'cached', value: partitions.cached },
  ]
  if (partitions.empty) figures.push({ id: 'empty', label: 'wrote 0 rows', value: partitions.empty })
  if (lastRun && lastRun.keys > 0) {
    figures.push({ id: 'rebuilt', label: 'rebuilt in the last run', value: Math.max(lastRun.keys - lastRun.failed, 0) })
    figures.push({ id: 'failed', label: 'failed in the last run', value: lastRun.failed })
  }
  return figures
}

/** `1,205 rows`, `1 row`; null when barca did not count them. */
export function rowsLabel(rows: number | null | undefined): string | null {
  if (rows === null || rows === undefined) return null
  return `${rows.toLocaleString()} ${rows === 1 ? 'row' : 'rows'}`
}

/**
 * Which output a node's Data tab inspects, and why. A node without keys has one output. A
 * partitioned node's is one key: the newest key recorded when it succeeded; when that key's
 * last attempt failed, the result the key still has cached from an earlier run (if any), never
 * an error in place of data.
 */
export type DataKey =
  | { kind: 'whole' }
  | { kind: 'none' }
  | { kind: 'newest'; key: string }
  | { kind: 'cached'; key: string }

export function dataKey(node: Pick<NodeState, 'partitioned' | 'last_materialization'>): DataKey {
  if (!node.partitioned) return { kind: 'whole' }
  const last = node.last_materialization
  if (!last?.partition) return { kind: 'none' }
  return last.status === 'success' ? { kind: 'newest', key: last.partition } : { kind: 'cached', key: last.partition }
}

/** The sentence over a partitioned node's output in the Data tab: which key, and why that one. */
export function dataKeyNote(choice: DataKey, hasCached: boolean | null): string | null {
  return match(choice)
    .with({ kind: 'whole' }, () => null)
    .with({ kind: 'none' }, () => 'No key has been recorded yet.')
    .with({ kind: 'newest' }, ({ key }) => `Key ${key}: the newest key recorded, which succeeded.`)
    .with({ kind: 'cached' }, ({ key }) =>
      hasCached === null
        ? `Key ${key}: the newest key recorded. Its last attempt failed; looking for its cached result…`
        : hasCached
          ? `Key ${key}: the newest key recorded. Its last attempt failed, so this is the result it still has cached from an earlier run.`
          : `Key ${key}: the newest key recorded. Its last attempt failed and it has no cached result; open the key explorer to inspect another key.`)
    .exhaustive()
}

/**
 * What a Run of a partitioned node computes, when some keys need it: only those, when others are
 * cached; every key, when none is (`Run computes all 1,205 keys; none is cached`). Null when
 * nothing needs computing.
 */
export function runScopeNote(needed: number, total: number): string | null {
  if (needed <= 0) return null
  if (needed >= total) return total === 1 ? 'Run computes its one key; it has no cached result.' : `Run computes all ${total.toLocaleString()} keys; none is cached.`
  return `Run computes only these ${plural(needed, 'key')}; the other ${plural(total - needed, 'key')} stay cached.`
}
