/**
 * The Runs page as a program (`lib/program.ts`). The selected run and the filters live in the
 * URL (`?run=`, `?status=`, `?q=`, `?hour=`, `?schedule=hide`), so changing one is an effect (a
 * navigation), and the page reads it back as input.
 * Pure; `pages/RunsPage.tsx` runs it and `pages/RunsView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import { only, type Next } from './program'
import { parseRunStatus, runIsLive } from './runs'
import { formatHour, hourOf, parseTime, zoneName, type TimeZone } from './time'
import type { ActivityHour, RunList, RunNodeSummary, RunStep, RunSummary, StatusKind } from './types'

const RUNS_PAGE_SIZE = 100
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
  /** Leaves the schedule's runs out of the list and the activity: the server filters them. */
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
  | { type: 'hour_toggled'; at: number }
  /** The "Hide schedule runs" control. */
  | { type: 'schedule_toggled' }

type RunsEffect =
  /** Set `?run=` (`null` removes it); `replace` keeps the history entry. */
  | { type: 'navigate'; run: string | null; replace: boolean }
  /** Write the filters to the URL, replacing the history entry. */
  | { type: 'filters'; filters: RunFilters }
  | { type: 'refetch_list' }
  | { type: 'refetch_detail' }

/** The part of the model the URL owns. */
type RunFilters = Pick<RunsModel, 'filter' | 'query' | 'hour' | 'hideSchedule'>

export const INITIAL_RUNS: RunsModel = { limit: RUNS_PAGE_SIZE, filter: 'all', query: '', hour: null, hideSchedule: false }

/**
 * `triggered_by` of `GET /runs` for the list: every trigger but the schedule's when its runs
 * are hidden (`barca history --triggered-by manual,api,client`), else no filter.
 */
export function runsTriggeredBy(model: Pick<RunsModel, 'hideSchedule'>): string | null {
  return model.hideSchedule ? 'manual,api,client' : null
}

/** The filters a URL names; anything it cannot mean is the default. */
export function parseRunFilters(params: URLSearchParams): RunFilters {
  const status = params.get('status')
  const hour = Number(params.get('hour'))
  return {
    filter: status === 'failed' || status === 'live' ? status : 'all',
    query: params.get('q') ?? '',
    hour: params.get('hour') !== null && Number.isFinite(hour) ? hour : null,
    hideSchedule: params.get('schedule') === 'hide',
  }
}

/** `params` with `filters` written; a default leaves its parameter out. */
export function writeRunFilters(params: URLSearchParams, { filter, query, hour, hideSchedule }: RunFilters): URLSearchParams {
  const next = new URLSearchParams(params)
  const put = (key: string, value: string | null) => (value ? next.set(key, value) : next.delete(key))
  put('status', filter === 'all' ? null : filter)
  put('q', query || null)
  put('hour', hour === null ? null : String(hour))
  put('schedule', hideSchedule ? 'hide' : null)
  return next
}

const withFilters = (model: RunsModel, change: Partial<RunFilters>): Next<RunsModel, RunsEffect> => {
  const next = { ...model, ...change }
  return [next, [{ type: 'filters', filters: { filter: next.filter, query: next.query, hour: next.hour, hideSchedule: next.hideSchedule } }]]
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
    .with({ type: 'schedule_toggled' }, () => withFilters(model, { hideSchedule: !model.hideSchedule }))
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

/** Whether the list is narrowed by a filter, a search or an hour. */
function narrowed(model: Pick<RunsModel, 'filter' | 'query' | 'hour'>): boolean {
  return model.filter !== 'all' || model.query.trim() !== '' || model.hour !== null
}

/**
 * The Runs header's count. Unnarrowed, every run the server has (`37,955`). Narrowed, the count
 * is over the runs loaded, so when those are not every run it says so
 * (`21 not successful in the latest 100`) rather than reading as a share of all history
 * (`GET /runs` counts only all runs; a filtered count would need the server to count it).
 */
export function runsCountLabel(
  list: Pick<RunList, 'runs' | 'total' | 'truncated'>,
  shown: number,
  model: Pick<RunsModel, 'filter' | 'query' | 'hour'>,
): { text: string; title: string } {
  const loaded = list.runs.length
  const runs = (n: number) => `${n.toLocaleString()} run${n === 1 ? '' : 's'}`
  if (!narrowed(model)) return { text: list.total.toLocaleString(), title: runs(list.total) }
  if (!list.truncated) return { text: `${shown.toLocaleString()} of ${list.total.toLocaleString()}`, title: `${shown.toLocaleString()} of ${runs(list.total)}` }
  const word = match(model.filter)
    .with('failed', () => 'not successful')
    .with('live', () => 'in progress')
    .with('all', () => 'matching')
    .exhaustive()
  return {
    text: `${shown.toLocaleString()} ${word} in the latest ${loaded.toLocaleString()}`,
    title: `Counted over the latest ${runs(loaded)} loaded, of ${list.total.toLocaleString()} in all; Load more counts further back`,
  }
}

/** The "Not successful" chip's count, and what it is counted over. */
export function failedChipCount(list: Pick<RunList, 'runs' | 'truncated'>): { count: number; title: string | undefined } {
  const count = list.runs.filter((r) => runFailed(r.status)).length
  const n = list.runs.length
  return { count, title: list.truncated ? `${count.toLocaleString()} not successful in the latest ${n.toLocaleString()} run${n === 1 ? '' : 's'} loaded` : undefined }
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
    const { failed, ok } = splitRunSteps(group.steps)
    for (const step of failed) rows.push({ kind: 'key', node, step })
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

/** A bar's tooltip: the hour (zone named) and its runs, as the server counted them. */
export function activityBarLabel(h: ActivityHour, zone: TimeZone = 'local'): string {
  const at = activityHourAt(h)
  const parts = [`${hourLabel(at, zone)} ${zoneName(zone, at)}: ${h.runs} run${h.runs === 1 ? '' : 's'}`]
  if (h.not_successful) parts.push(`${h.not_successful} not successful`)
  if (h.in_progress) parts.push(`${h.in_progress} in progress`)
  return parts.join(', ')
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
