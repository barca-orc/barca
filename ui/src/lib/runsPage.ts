/**
 * The Runs page as a program (`lib/program.ts`). The selected run lives in the URL (`?run=`),
 * so selecting one is an effect (a navigation), and the page reads it back as input.
 * Pure; `pages/RunsPage.tsx` runs it and `pages/RunsView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { only, type Next } from './program'
import { parseRunStatus, runIsLive } from './runs'
import type { RunSummary } from './types'

export const RUNS_PAGE_SIZE = 100
export const RUNS_MAX = 1000

/** Which runs the list shows. */
export type RunFilter = 'all' | 'failed' | 'live'

export interface RunsModel {
  /** How many runs the list asks for. */
  limit: number
  filter: RunFilter
  /** Narrows the list to runs whose target or id contains it. */
  query: string
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

export type RunsEffect =
  /** Set `?run=` (`null` removes it); `replace` keeps the history entry. */
  | { type: 'navigate'; run: string | null; replace: boolean }
  | { type: 'refetch_list' }
  | { type: 'refetch_detail' }

export const INITIAL_RUNS: RunsModel = { limit: RUNS_PAGE_SIZE, filter: 'all', query: '' }

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
export function visibleRuns(runs: readonly RunSummary[], model: Pick<RunsModel, 'filter' | 'query'>): RunSummary[] {
  const needle = model.query.trim().toLowerCase()
  return runs.filter((run) =>
    (model.filter === 'all' || (model.filter === 'failed' ? runFailed(run.status) : runIsLive(run.status))) &&
    (!needle || `${run.id} ${run.target ?? 'all assets'}`.toLowerCase().includes(needle)),
  )
}

/** One hour of the activity strip. */
export interface ActivityHour {
  /** Start of the hour, ms since epoch. */
  at: number
  ok: number
  failed: number
  live: number
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
  const hour = 3_600_000
  const end = Math.floor(nowMs / hour) * hour + hour
  const start = end - hours * hour
  const buckets: ActivityHour[] = Array.from({ length: hours }, (_, i) => ({ at: start + i * hour, ok: 0, failed: 0, live: 0 }))
  const durations: number[] = []
  let oldest = Infinity
  for (const run of runs) {
    const at = Date.parse(run.started_at)
    if (Number.isNaN(at)) continue
    oldest = Math.min(oldest, at)
    if (at < start || at >= end) continue
    const bucket = buckets[Math.floor((at - start) / hour)]!
    if (runIsLive(run.status)) bucket.live += 1
    else if (runFailed(run.status)) bucket.failed += 1
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
