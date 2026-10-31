/**
 * The Runs page as a program (`lib/program.ts`). The selected run and the filters live in the
 * URL (`?run=`, `?status=`, `?q=`, `?hour=`), so changing one is an effect (a navigation), and
 * the page reads it back as input.
 * Pure; `pages/RunsPage.tsx` runs it and `pages/RunsView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import { headline } from './health'
import { only, type Next } from './program'
import { parseRunStatus, runIsLive } from './runs'
import { parseTime } from './time'
import type { FirstFailure, RunNodeSummary, RunStep, RunSummary, RunView, StatusKind } from './types'

export const RUNS_PAGE_SIZE = 100
export const RUNS_MAX = 1000

const HOUR = 3_600_000

/** Which runs the list shows. */
export type RunFilter = 'all' | 'failed' | 'live'

export interface RunsModel {
  /** How many runs the list asks for. */
  limit: number
  filter: RunFilter
  /** Narrows the list to runs whose target or id contains it. */
  query: string
  /** Narrows the list to the runs started in this hour (its start, ms since epoch). */
  hour: number | null
}

export type RunsMsg =
  | { type: 'run_selected'; id: string }
  | { type: 'panel_closed' }
  /** The selected run's details name its durable run id, which differs from the link's. */
  | { type: 'run_resolved'; id: string }
  | { type: 'load_more' }
  | { type: 'retry_list' }
  | { type: 'retry_detail' }
  | { type: 'filter_changed'; filter: RunFilter }
  | { type: 'query_changed'; value: string }
  /** A bar of the activity strip: the same one again clears the filter. */
  | { type: 'hour_toggled'; at: number }

export type RunsEffect =
  /** Set `?run=` (`null` removes it); `replace` keeps the history entry. */
  | { type: 'navigate'; run: string | null; replace: boolean }
  /** Write the filters to the URL, replacing the history entry. */
  | { type: 'filters'; filters: RunFilters }
  | { type: 'refetch_list' }
  | { type: 'refetch_detail' }

/** The part of the model the URL owns. */
export type RunFilters = Pick<RunsModel, 'filter' | 'query' | 'hour'>

export const INITIAL_RUNS: RunsModel = { limit: RUNS_PAGE_SIZE, filter: 'all', query: '', hour: null }

/** The filters a URL names; anything it cannot mean is the default. */
export function parseRunFilters(params: URLSearchParams): RunFilters {
  const status = params.get('status')
  const hour = Number(params.get('hour'))
  return {
    filter: status === 'failed' || status === 'live' ? status : 'all',
    query: params.get('q') ?? '',
    hour: params.get('hour') !== null && Number.isFinite(hour) ? hour : null,
  }
}

/** `params` with `filters` written; a default leaves its parameter out. */
export function writeRunFilters(params: URLSearchParams, { filter, query, hour }: RunFilters): URLSearchParams {
  const next = new URLSearchParams(params)
  const put = (key: string, value: string | null) => (value ? next.set(key, value) : next.delete(key))
  put('status', filter === 'all' ? null : filter)
  put('q', query || null)
  put('hour', hour === null ? null : String(hour))
  return next
}

const withFilters = (model: RunsModel, change: Partial<RunFilters>): Next<RunsModel, RunsEffect> => {
  const next = { ...model, ...change }
  return [next, [{ type: 'filters', filters: { filter: next.filter, query: next.query, hour: next.hour } }]]
}

/** Whether "Load more" can ask for more runs. */
export function canLoadMore(model: RunsModel): boolean {
  return model.limit < RUNS_MAX
}

export function updateRuns(model: RunsModel, msg: RunsMsg): Next<RunsModel, RunsEffect> {
  return match<RunsMsg, Next<RunsModel, RunsEffect>>(msg)
    .with({ type: 'run_selected' }, ({ id }) => [model, [{ type: 'navigate', run: id, replace: false }]])
    .with({ type: 'panel_closed' }, () => [model, [{ type: 'navigate', run: null, replace: false }]])
    // Replace an older link's temporary server handle with the run id, so bookmarked
    // details keep working after the server restarts.
    .with({ type: 'run_resolved' }, ({ id }) => [model, [{ type: 'navigate', run: id, replace: true }]])
    .with({ type: 'load_more' }, () => only({ ...model, limit: Math.min(model.limit + RUNS_PAGE_SIZE, RUNS_MAX) }))
    .with({ type: 'filter_changed' }, ({ filter }) => withFilters(model, { filter }))
    .with({ type: 'query_changed' }, ({ value }) => withFilters(model, { query: value }))
    .with({ type: 'hour_toggled' }, ({ at }) => withFilters(model, { hour: model.hour === at ? null : at }))
    .with({ type: 'retry_list' }, () => [model, [{ type: 'refetch_list' }]])
    .with({ type: 'retry_detail' }, () => [model, [{ type: 'refetch_detail' }]])
    .exhaustive()
}

/** A run that did not succeed: failed, errored, cancelled or interrupted. */
export function runFailed(status: string): boolean {
  const known = parseRunStatus(status)
  return known !== null && known !== 'success' && !runIsLive(status)
}

/** The runs the list shows for `model`'s filter and query. */
export function visibleRuns(runs: readonly RunSummary[], model: Pick<RunsModel, 'filter' | 'query' | 'hour'>): RunSummary[] {
  const needle = model.query.trim().toLowerCase()
  return runs.filter((run) =>
    (model.filter === 'all' || (model.filter === 'failed' ? runFailed(run.status) : runIsLive(run.status))) &&
    (!needle || `${run.id} ${run.target ?? 'all assets'}`.toLowerCase().includes(needle)) &&
    (model.hour === null || inHour(run, model.hour)),
  )
}

function inHour(run: RunSummary, at: number): boolean {
  const started = parseTime(run.started_at)
  return started !== null && started >= at && started < at + HOUR
}

/** A step's short name, with its partition key: `pipeline.py:a[day=1]` → `a[day=1]`. */
export function stepName(nodeId: string): string {
  const key = nodeId.indexOf('[')
  return key < 0 ? shortName(nodeId) : shortName(nodeId.slice(0, key)) + nodeId.slice(key)
}

/** `orders: KeyError: 'x'`: the first failure in a line. */
function failureLine(failure: FirstFailure): string {
  return `${stepName(failure.node_id)}: ${headline(failure.error)}`
}

/** A row's error line: the run's own error, else its first failed step; null with nothing to say. */
export function runRowError(run: RunSummary): { text: string; title: string } | null {
  if (run.error) return { text: headline(run.error), title: run.error }
  if (!runFailed(run.status)) return null
  if (run.first_failure) return { text: failureLine(run.first_failure), title: `${run.first_failure.node_id}: ${run.first_failure.error}` }
  return parseRunStatus(run.status) === 'failed' ? { text: 'No failed step recorded: open the run', title: 'No failed step recorded: open the run' } : null
}

/**
 * What the row says went wrong: where it stopped and why, or its own error. A partitioned
 * node's failure counts its keys: `demand_forecast: 7 keys failed · BatchKeyFailed: …`.
 */
export function runFailureLine(run: RunSummary): string | null {
  const error = runRowError(run)
  if (!error) return null
  if (!run.first_failure) return error.text
  const why = headline(run.error ?? run.first_failure.error)
  const key = run.first_failure.node_id.indexOf('[')
  if (key < 0) return `stopped at ${stepName(run.first_failure.node_id)} · ${why}`
  const failed = Math.max(run.steps_failed ?? 1, 1)
  return `${shortName(run.first_failure.node_id.slice(0, key))}: ${failed.toLocaleString()} key${failed === 1 ? '' : 's'} failed · ${why}`
}

/** `3,077 ran · 3 cached`: a run's steps (keys) in one unit; `0 ran · 9,236 cached` when all cached. */
export function stepsLabel(executed: number, cached: number): string {
  return `${executed.toLocaleString()} ran · ${cached.toLocaleString()} cached`
}

/** How far a run got, in steps, and the step it stopped at when it did not finish. */
export function runProgress(run: RunSummary): { steps: string; stoppedAt: string | null; title: string } {
  const total = run.steps_total
  const planned = total === null ? [] : [`${total.toLocaleString()} node${total === 1 ? '' : 's'} planned`]
  const stopped = run.first_failure ? [`stopped at ${run.first_failure.node_id}`] : []
  return {
    steps: stepsLabel(run.steps_executed, run.steps_cached),
    stoppedAt: run.first_failure && runFailed(run.status) ? stepName(run.first_failure.node_id) : null,
    title: ['Steps are keys: a partitioned node counts one per key', ...planned, ...stopped].join(' · '),
  }
}

/** A run's steps by node: its totals, and the keys the page lists, failed ones first. */
export interface RunNodeGroup {
  summary: RunNodeSummary
  steps: RunStep[]
}

/**
 * Group a run's listed steps under the summary of every step it recorded (`steps_page.nodes`). A
 * page lists at most some of a node's keys, so counts come from the summary, never the page.
 */
export function groupRunSteps(steps: readonly RunStep[], nodes: readonly RunNodeSummary[]): RunNodeGroup[] {
  const byNode = new Map<string, RunStep[]>()
  for (const step of steps) {
    const key = step.node_id.indexOf('[')
    const base = key < 0 ? step.node_id : step.node_id.slice(0, key)
    const listed = byNode.get(base)
    if (listed) listed.push(step)
    else byNode.set(base, [step])
  }
  return nodes.map((summary) => ({
    summary,
    steps: (byNode.get(summary.node_id) ?? []).slice().sort((a, b) => Number(b.status === 'failed') - Number(a.status === 'failed')),
  }))
}

/** `3,077 keys · 40 failed · 7 calls`. */
export function runNodeLine(n: RunNodeSummary): string {
  const parts = [`${n.keys.toLocaleString()} key${n.keys === 1 ? '' : 's'}`]
  if (n.failed) parts.push(`${n.failed.toLocaleString()} failed`)
  if (n.calls) parts.push(`${n.calls.toLocaleString()} call${n.calls === 1 ? '' : 's'}`)
  return parts.join(' · ')
}

/** Whether the node recorded no step this run: it was cached, or skipped. */
export function runNodeIdle(n: RunNodeSummary): boolean {
  return n.status === 'cached' || n.status === 'skipped'
}

/**
 * A node's badge and line: `3,077 keys · 7 failed · 5 calls` for a node that ran, `cached`, or
 * `skipped` with why (`upstream demand_forecast failed`, or the run stopped before it).
 */
export function runNodeOutcome(n: RunNodeSummary): { kind: StatusKind; label: string; detail: string | null } {
  if (n.status === 'cached') return { kind: 'skipped', label: 'cached', detail: null }
  if (n.status === 'skipped') {
    return { kind: 'skipped', label: 'skipped', detail: n.blocked_by ? `upstream ${shortName(n.blocked_by)} failed` : 'the run stopped before it' }
  }
  return { kind: n.failed ? 'failed' : 'success', label: runNodeLine(n), detail: null }
}

/**
 * A node's time, as the summary records it: summed over its keys, so a batched node's time is
 * `2.2s over 5 calls`, which ran in parallel and can add up to more than the run took.
 */
export function runNodeTime(n: RunNodeSummary, format: (seconds: number) => string): { text: string; title: string } | null {
  if (runNodeIdle(n)) return null
  if (n.calls > 1) {
    return {
      text: `${format(n.elapsed_seconds)} over ${n.calls.toLocaleString()} calls`,
      title: `Compute time summed over ${n.calls.toLocaleString()} batched calls, which ran in parallel`,
    }
  }
  if (n.keys > 1) return { text: format(n.elapsed_seconds), title: `Compute time summed over ${n.keys.toLocaleString()} keys` }
  return { text: format(n.elapsed_seconds), title: 'Compute time' }
}

/** A run's listed steps by outcome: the failed ones (first), then the rest. */
export function splitRunSteps(steps: readonly RunStep[]): { failed: RunStep[]; ok: RunStep[] } {
  return { failed: steps.filter((s) => s.status === 'failed'), ok: steps.filter((s) => s.status !== 'failed') }
}

/**
 * What became of each target, one line each, from the run's planned nodes:
 * `publish_plan did not run: upstream demand_forecast failed 7 keys`. Empty when the nodes carry
 * no status (the server could not rebuild the run's plan), or the run has no named target.
 */
export function runTargetLines(run: Pick<RunView, 'target' | 'job'>, nodes: readonly RunNodeSummary[]): string[] {
  if (!nodes.some((n) => n.status)) return []
  const ids = run.job && run.job !== 'all' ? run.job.split(',') : (run.target?.split(',') ?? [])
  const byShort = new Map(nodes.map((n) => [shortName(n.node_id), n]))
  return ids.flatMap((id) => {
    const name = shortName(id.trim())
    const node = nodes.find((n) => n.node_id === id.trim()) ?? byShort.get(name)
    if (!node) return []
    const keys = (n: number) => `${n.toLocaleString()} key${n === 1 ? '' : 's'}`
    if (node.status === 'skipped') {
      if (!node.blocked_by) return [`${name} did not run: the run stopped before it`]
      const up = nodes.find((n) => n.node_id === node.blocked_by)
      const failed = up && up.keys > 1 ? ` ${keys(up.failed)}` : ''
      return [`${name} did not run: upstream ${shortName(node.blocked_by)} failed${failed}`]
    }
    if (node.status === 'cached') return [`${name} was cached: nothing to do`]
    if (node.failed) return [node.keys > 1 ? `${name} failed: ${node.failed.toLocaleString()} of ${keys(node.keys)}` : `${name} failed`]
    return [node.keys > 1 ? `${name} ran: ${keys(node.keys)}` : `${name} ran`]
  })
}

/** One hour of the activity strip. */
export interface ActivityHour {
  /** Start of the hour, ms since epoch. */
  at: number
  ok: number
  failed: number
  live: number
  /** The first failed step of the newest failed run of the hour. */
  firstFailure: FirstFailure | null
}

/** What the loaded runs say about the last day. */
export interface RunActivity {
  hours: ActivityHour[]
  total: number
  failed: number
  /** Median wall time of the finished runs, seconds; null with none. */
  medianSeconds: number | null
  /** The oldest loaded run is newer than the window: the counts cover only what was loaded. */
  partial: boolean
}

/** Runs started in the `hours` before `nowMs`, hour by hour, oldest first. */
export function runActivity(runs: readonly RunSummary[], nowMs: number, hours = 24, truncated = false): RunActivity {
  const end = Math.floor(nowMs / HOUR) * HOUR + HOUR
  const start = end - hours * HOUR
  const buckets: ActivityHour[] = Array.from({ length: hours }, (_, i) => ({ at: start + i * HOUR, ok: 0, failed: 0, live: 0, firstFailure: null }))
  const durations: number[] = []
  let oldest = Infinity
  for (const run of runs) {
    const at = parseTime(run.started_at)
    if (at === null) continue
    oldest = Math.min(oldest, at)
    if (at < start || at >= end) continue
    const bucket = buckets[Math.floor((at - start) / HOUR)]!
    if (runIsLive(run.status)) bucket.live += 1
    else if (runFailed(run.status)) {
      bucket.failed += 1
      bucket.firstFailure ??= run.first_failure
    }
    else bucket.ok += 1
    if (run.elapsed_seconds !== null && !runIsLive(run.status)) durations.push(run.elapsed_seconds)
  }
  durations.sort((a, b) => a - b)
  const total = buckets.reduce((n, b) => n + b.ok + b.failed + b.live, 0)
  return {
    hours: buckets,
    total,
    failed: buckets.reduce((n, b) => n + b.failed, 0),
    medianSeconds: durations.length ? durations[Math.floor((durations.length - 1) / 2)]! : null,
    partial: truncated && oldest > start,
  }
}

/** `14:00`, in the reader's own time zone. */
export function hourLabel(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
}

/** A bar's tooltip: the hour, its runs, and what failed first. */
export function activityBarLabel(h: ActivityHour): string {
  const n = h.ok + h.failed + h.live
  const parts = [`${hourLabel(h.at)}: ${n} run${n === 1 ? '' : 's'}`]
  if (h.failed) parts.push(`${h.failed} not successful`)
  if (h.live) parts.push(`${h.live} in progress`)
  const failure = h.firstFailure ? `\n${failureLine(h.firstFailure)}` : ''
  return parts.join(', ') + failure
}

/** A tick every `every` hours of local time: an hour's bar index with its label. */
export function activityTicks(hours: readonly ActivityHour[], every = 6): { index: number; label: string }[] {
  return hours.flatMap((h, index) =>
    new Date(h.at).getHours() % every === 0 && index < hours.length - 2 ? [{ index, label: hourLabel(h.at) }] : [],
  )
}
