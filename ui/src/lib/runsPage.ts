/**
 * The Runs page as a program (`lib/program.ts`). The selected run and the filters live in the
 * URL (`?run=`, `?status=`, `?q=`, `?since=`/`?until=`, `?schedule=`), so changing one is an
 * effect (a navigation), and the page reads it back as input. Every filter is the server's: the
 * model becomes the query of `GET /runs` (`runsQuery`), and the list, its count and the
 * activity come back filtered. Nothing here filters or counts runs.
 * Pure; `pages/RunsPage.tsx` runs it and `pages/RunsView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import type { RunsQuery } from './types'
import { only, type Next } from './program'
import { isFailure, isSkipped, parseRunStatus, runNodeIdle } from './runs'
import { formatHour, hourOf, parseTime, zoneName, type TimeZone, formatSeconds } from './time'
import type { ActivityHour, RunNodeSummary, RunStep } from './types'

const RUNS_PAGE_SIZE = 100
export const RUNS_MAX = 1000

/** Which runs the list shows. */
export type RunFilter = 'all' | 'failed' | 'live'

/** The runs started in a window: an hour of the activity, as the server names it (`at`, `until`). */
export interface RunWindow {
  since: string
  until: string
}

export interface RunsModel {
  /** How many runs the list asks for. */
  limit: number
  filter: RunFilter
  /** Narrows the list to runs whose run id, target or job contains it. */
  query: string
  /** Narrows the list to the runs started in one hour of the activity. */
  window: RunWindow | null
  /** Leaves the schedule's runs out of the list and the activity. */
  hideSchedule: boolean
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
  | { type: 'hour_toggled'; window: RunWindow }
  /** The "Hide schedule runs" control. */
  | { type: 'schedule_toggled' }
  /** The hidden schedule runs that did not succeed: shows them, and only them. */
  | { type: 'schedule_failures_shown' }
  /** Back to the default view. */
  | { type: 'filters_cleared' }

type RunsEffect =
  /** Set `?run=` (`null` removes it); `replace` keeps the history entry. */
  | { type: 'navigate'; run: string | null; replace: boolean }
  /**
   * Write what changed of the filters to the URL, replacing the history entry. Only the
   * change: two messages before the page reads the URL back must not undo one another.
   */
  | { type: 'filters'; change: Partial<RunFilters> }
  | { type: 'refetch_list' }
  | { type: 'refetch_detail' }

/** The part of the model the URL owns. */
export type RunFilters = Pick<RunsModel, 'filter' | 'query' | 'window' | 'hideSchedule'>

/**
 * The default view hides the schedule's runs: on a server whose history is mostly ticks, a
 * person's own runs are what the page is opened for, and what was hidden is said beside the
 * list with its failures (`activity.excluded`).
 */
export const HIDE_SCHEDULE_BY_DEFAULT = true

export const INITIAL_RUNS: RunsModel = { limit: RUNS_PAGE_SIZE, filter: 'all', query: '', window: null, hideSchedule: HIDE_SCHEDULE_BY_DEFAULT }

/** The newest `limit` runs, unfiltered: what `GET /runs` answers without a filter. */
export function everyRun(limit: number): RunsQuery {
  return { limit, status: null, triggered_by: null, not_triggered_by: null, since: null, until: null, search: null }
}

/** What the page asks the server for: the model's filters as `GET /runs` names them. */
export function runsQuery(model: RunsModel): RunsQuery {
  return {
    limit: model.limit,
    status: match(model.filter)
      .with('failed', () => 'not_successful' as const)
      .with('live', () => 'running' as const)
      .with('all', () => null)
      .exhaustive(),
    triggered_by: null,
    not_triggered_by: model.hideSchedule ? 'schedule' : null,
    since: model.window?.since ?? null,
    until: model.window?.until ?? null,
    search: model.query.trim() || null,
  }
}

/** `barca history` with the same filters: what the page shows, in a terminal. */
export function runsCommand(model: RunsModel): string {
  const q = runsQuery(model)
  const flags = [
    q.status && `--status ${q.status}`,
    q.not_triggered_by && `--not-triggered-by ${q.not_triggered_by}`,
    q.since && `--since ${q.since}`,
    q.until && `--until ${q.until}`,
    q.search && `--search ${/^[\w.:=-]+$/.test(q.search) ? q.search : JSON.stringify(q.search)}`,
  ].filter(Boolean)
  return ['barca history', ...flags].join(' ')
}

/**
 * The filters a URL names; anything it cannot mean is the default. `hideSchedule` is this
 * browser's last choice (`HIDE_SCHEDULE_BY_DEFAULT` before it made one), unless the URL says.
 */
export function parseRunFilters(params: URLSearchParams, hideSchedule = HIDE_SCHEDULE_BY_DEFAULT): RunFilters {
  const status = params.get('status')
  const since = params.get('since')
  const until = params.get('until')
  const schedule = params.get('schedule')
  return {
    filter: status === 'failed' || status === 'live' ? status : 'all',
    query: params.get('q') ?? '',
    window: since && until && parseTime(since) !== null && parseTime(until) !== null ? { since, until } : null,
    hideSchedule: schedule === 'show' ? false : schedule === 'hide' ? true : hideSchedule,
  }
}

/**
 * `params` with `filters` written; a default leaves its parameter out. Showing the schedule's
 * runs is always written (`schedule=show`), so a link opens the view it was copied from.
 */
export function writeRunFilters(params: URLSearchParams, { filter, query, window, hideSchedule }: RunFilters): URLSearchParams {
  const next = new URLSearchParams(params)
  const put = (key: string, value: string | null) => (value ? next.set(key, value) : next.delete(key))
  put('status', filter === 'all' ? null : filter)
  put('q', query || null)
  put('since', window?.since ?? null)
  put('until', window?.until ?? null)
  put('schedule', hideSchedule === HIDE_SCHEDULE_BY_DEFAULT ? null : 'show')
  return next
}

const withFilters = (model: RunsModel, change: Partial<RunFilters>): Next<RunsModel, RunsEffect> =>
  [{ ...model, ...change }, [{ type: 'filters', change }]]

/** Whether "Load more" can ask for more runs. */
export function canLoadMore(model: RunsModel): boolean {
  return model.limit < RUNS_MAX
}

/** Whether the list is narrowed by a status, a search or an hour (the schedule's runs apart). */
export function runsNarrowed(model: Pick<RunsModel, 'filter' | 'query' | 'window'>): boolean {
  return model.filter !== 'all' || model.query.trim() !== '' || model.window !== null
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
    .with({ type: 'hour_toggled' }, ({ window }) => withFilters(model, { window: model.window?.since === window.since ? null : window }))
    .with({ type: 'schedule_toggled' }, () => withFilters(model, { hideSchedule: !model.hideSchedule }))
    .with({ type: 'schedule_failures_shown' }, () => withFilters(model, { hideSchedule: false, filter: 'failed' }))
    .with({ type: 'filters_cleared' }, () => withFilters(model, { filter: 'all', query: '', window: null }))
    .with({ type: 'retry_list' }, () => [model, [{ type: 'refetch_list' }]])
    .with({ type: 'retry_detail' }, () => [model, [{ type: 'refetch_detail' }]])
    .exhaustive()
}

/** Whether the recovery action should offer to re-run failed steps. */
export function rerunLabel(status: string): string | null {
  const known = parseRunStatus(status)
  if (known === 'failed') return 'Re-run failed'
  if (known === 'cancelled' || known === 'interrupted') return 'Re-run'
  return null
}

/** A step's short name, with its partition key: `pipeline.py:a[day=1]` → `a[day=1]`. */
export function stepName(nodeId: string): string {
  const key = nodeId.indexOf('[')
  return key < 0 ? shortName(nodeId) : shortName(nodeId.slice(0, key)) + nodeId.slice(key)
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
    steps: (byNode.get(summary.node_id) ?? []).slice().sort((a, b) => Number(isFailure(b.status)) - Number(isFailure(a.status))),
  }))
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
  return { failed: steps.filter((s) => isFailure(s.status)), ok: steps.filter((s) => !isFailure(s.status)) }
}

/**
 * One row of a run's Steps list, which the panel renders only in view (`RunsView`'s
 * `RunSteps`), so a run of 60,000 planned nodes and 1,000 listed keys stays a few dozen rows
 * in the DOM. A node's keys are one row with their count; opening it lists them under it.
 */
export type RunStepRow =
  /** A planned node that recorded no step: cached, or skipped. */
  | { kind: 'idle'; summary: RunNodeSummary }
  /** A node of one key and one listed step: the node's row is the step's. */
  | { kind: 'single'; summary: RunNodeSummary; step: RunStep }
  /** A node of several keys (or whose keys the page does not list): its count, opened or not. */
  | { kind: 'node'; group: RunNodeGroup; open: boolean }
  /** One listed key of an opened node. */
  | { kind: 'key'; node: string; step: RunStep }
  /** An opened node's successful keys, behind their count when it also has failed ones. */
  | { kind: 'ok_keys'; node: string; count: number; open: boolean }
  /** An opened node's keys the page does not list. */
  | { kind: 'unlisted'; node: string; count: number }
  /** A step of a run without a per-node summary (no partitioned node): one row each. */
  | { kind: 'step'; step: RunStep }

/** Which nodes of the Steps list are open, and which of those show their successful keys. */
export interface RunStepsOpen {
  nodes: ReadonlySet<string>
  ok: ReadonlySet<string>
}

export const NO_STEPS_OPEN: RunStepsOpen = { nodes: new Set(), ok: new Set() }

/** `open` with `id` opened or closed in `which`. */
export function toggleStepsOpen(open: RunStepsOpen, which: keyof RunStepsOpen, id: string): RunStepsOpen {
  const next = new Set(open[which])
  if (next.has(id)) next.delete(id)
  else next.add(id)
  return { ...open, [which]: next }
}

/**
 * The Steps list's rows: a row per planned node (`groupRunSteps`), and under each opened one its
 * failed keys, then its successful ones (behind their count when some failed), then how many
 * keys the page leaves out. Without a per-node summary, a row per listed step.
 */
export function runStepRows(steps: readonly RunStep[], nodes: readonly RunNodeSummary[], open: RunStepsOpen): RunStepRow[] {
  if (nodes.length === 0) return steps.map((step) => ({ kind: 'step', step }))
  const rows: RunStepRow[] = []
  for (const group of groupRunSteps(steps, nodes)) {
    const { summary } = group
    if (runNodeIdle(summary)) {
      rows.push({ kind: 'idle', summary })
      continue
    }
    if (summary.keys <= 1 && group.steps.length === 1) {
      rows.push({ kind: 'single', summary, step: group.steps[0]! })
      continue
    }
    const opened = open.nodes.has(summary.node_id)
    rows.push({ kind: 'node', group, open: opened })
    if (!opened) continue
    const node = summary.node_id
    const skipped = group.steps.filter((s) => isSkipped(s.status))
    const { failed, ok } = splitRunSteps(group.steps.filter((s) => !isSkipped(s.status)))
    for (const step of failed) rows.push({ kind: 'key', node, step })
    for (const step of skipped) rows.push({ kind: 'key', node, step })
    if (ok.length > 0) {
      if (failed.length === 0) for (const step of ok) rows.push({ kind: 'key', node, step })
      else {
        const okOpen = open.ok.has(node)
        rows.push({ kind: 'ok_keys', node, count: ok.length, open: okOpen })
        if (okOpen) for (const step of ok) rows.push({ kind: 'key', node, step })
      }
    }
    const unlisted = summary.keys - group.steps.length
    if (unlisted > 0) rows.push({ kind: 'unlisted', node, count: unlisted })
  }
  return rows
}

/** The start of an hour of the server's activity (`activity.hours[].at`), ms since epoch. */
export function activityHourAt(h: Pick<ActivityHour, 'at'>): number {
  return parseTime(h.at) ?? 0
}

/** `14:00`, in the reader's chosen time zone. */
export function hourLabel(at: number, zone: TimeZone = 'local'): string {
  return formatHour(at, zone)
}

/** `14:00–15:00 UTC`: a window of the activity, in the reader's chosen time zone. */
export function windowLabel(window: RunWindow, zone: TimeZone = 'local'): string {
  const since = parseTime(window.since) ?? 0
  return `${hourLabel(since, zone)}–${hourLabel(parseTime(window.until) ?? since, zone)} ${zoneName(zone, since)}`
}

/**
 * A bar's tooltip, every figure the server's: the hour (zone named), its runs by outcome, the
 * median time of the ones that finished, and what most of the unsuccessful ones say went wrong.
 */
export function activityBarLabel(h: ActivityHour, zone: TimeZone = 'local'): string {
  const at = activityHourAt(h)
  const counts = [`${hourLabel(at, zone)} ${zoneName(zone, at)}: ${h.runs.toLocaleString()} run${h.runs === 1 ? '' : 's'}`]
  if (h.not_successful) counts.push(`${h.not_successful.toLocaleString()} not successful`)
  if (h.in_progress) counts.push(`${h.in_progress.toLocaleString()} in progress`)
  const parts = [counts.join(', ')]
  if (h.median_seconds !== null) parts.push(`median ${formatSeconds(h.median_seconds)}`)
  if (h.failure) parts.push(h.failure.runs === h.not_successful ? h.failure.text : `${h.failure.text} (${h.failure.runs.toLocaleString()} of ${h.not_successful.toLocaleString()})`)
  return parts.join(' · ')
}

/**
 * A tick every `every` hours of the reader's chosen zone: an hour's bar index with its label.
 * None in the last four hours, where `now` is: beside a side panel the chart is narrow.
 */
export function activityTicks(hours: readonly ActivityHour[], every = 6, zone: TimeZone = 'local'): { index: number; label: string }[] {
  return hours.flatMap((h, index) => {
    const at = activityHourAt(h)
    return hourOf(at, zone) % every === 0 && index < hours.length - 4 ? [{ index, label: hourLabel(at, zone) }] : []
  })
}
