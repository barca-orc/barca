/**
 * The Runs page as a program (`lib/program.ts`). The selected run lives in the URL (`?run=`),
 * so selecting one is an effect (a navigation), and the page reads it back as input.
 * Pure; `pages/RunsPage.tsx` runs it and `pages/RunsView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import { headline } from './health'
import { only, type Next } from './program'
import { parseRunStatus, runIsLive } from './runs'
import { parseTime } from './time'
import type { FirstFailure, RunSummary } from './types'

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
  | { type: 'refetch_list' }
  | { type: 'refetch_detail' }

export const INITIAL_RUNS: RunsModel = { limit: RUNS_PAGE_SIZE, filter: 'all', query: '', hour: null }

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
    .with({ type: 'filter_changed' }, ({ filter }) => only({ ...model, filter }))
    .with({ type: 'query_changed' }, ({ value }) => only({ ...model, query: value }))
    .with({ type: 'hour_toggled' }, ({ at }) => only({ ...model, hour: model.hour === at ? null : at }))
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

/** What the row says went wrong: where it stopped and why, or its own error. */
export function runFailureLine(run: RunSummary): string | null {
  const error = runRowError(run)
  if (!error) return null
  if (!run.first_failure) return error.text
  return `stopped at ${stepName(run.first_failure.node_id)} · ${headline(run.error ?? run.first_failure.error)}`
}

/** How far a run got: `21,950 / 28,404`, and the step it stopped at when it did not finish. */
export function runProgress(run: RunSummary): { steps: string; stoppedAt: string | null; title: string } {
  const done = run.steps_executed + run.steps_cached
  const total = run.steps_total
  return {
    steps: total === null ? done.toLocaleString() : `${done.toLocaleString()} / ${total.toLocaleString()}`,
    stoppedAt: run.first_failure && runFailed(run.status) ? stepName(run.first_failure.node_id) : null,
    title: `${run.steps_executed.toLocaleString()} ran · ${run.steps_cached.toLocaleString()} cached${run.first_failure ? ` · stopped at ${run.first_failure.node_id}` : ''}`,
  }
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
