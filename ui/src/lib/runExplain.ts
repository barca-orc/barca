/**
 * A run that explains itself (issue 925): why it executes what it does (`plan_reasons`), who asked
 * for it and why (`principal`, `trigger_reason`), why it was cancelled (`cancellation`), how
 * its attempts relate to its steps (`retries`), and, while it runs, how far it is, what it is
 * doing now and how long is left. Pure: `RunOverview` and `RunReasons` map these descriptors to
 * elements; `inFlightSummary` is the one line any page can show of a large run in flight.
 */

import { match } from 'ts-pattern'
import { elapsedLabel } from './running'
import { parseTime } from './time'
import { plural } from './plural'
import type {
  ActiveStep, Cancellation, NodeGroup, PlanReason, PlanReasons, ReasonCategory, RunNodeSummary,
  RunSummary, RunView, TriggeredBy,
} from './types'

// ── Why it executes what it does ──────────────────────────────────────────────

/** A reason category in words: what the engine decided, as an operator reads it. */
export function reasonLabel(category: ReasonCategory): string {
  return match(category)
    .with('never_run', () => 'never run')
    .with('new_keys', () => 'new partition keys')
    .with('failed', () => 'retrying a failed attempt')
    .with('code_changed', () => 'code changed')
    .with('inputs_changed', () => 'input changed')
    .with('env_changed', () => 'environment changed')
    .with('changed', () => 'changed (what was not recorded)')
    .with('refresh', () => 'named in --refresh')
    .with('refresh_cascade', () => 'downstream of a refresh')
    .with('refresh_all', () => '--refresh-all')
    .with('artifact_missing', () => 'artifact missing')
    .with('evicted', () => 'artifact evicted')
    .with('task', () => 'task (always runs)')
    .with('sensor', () => 'sensor (always runs)')
    .with('not_examined', () => 'not examined')
    .exhaustive()
}

/** What a reason's subject names, without the component word the category already says. */
function subjectText(reason: PlanReason): string | null {
  const subject = reason.subject
  if (subject === null) return null
  return match(reason.category)
    // `code`, `helper a.py:f`, `decorator argument inputs`: as `barca status` words them.
    .with('code_changed', () => subject)
    // `input a.py:x` → `a.py:x`; `sensor` and `value` keep their word.
    .with('inputs_changed', () => subject.replace(/^input /, ''))
    .with('env_changed', () => subject.replace(/^env /, ''))
    .with('never_run', 'new_keys', 'failed', 'changed', 'refresh', 'refresh_cascade', 'refresh_all',
      'artifact_missing', 'evicted', 'task', 'sensor', 'not_examined', () => subject)
    .exhaustive()
}

export interface ReasonRow {
  key: string
  category: ReasonCategory
  subject: string | null
  /** `12 nodes · 21,104 steps`. */
  counts: string
  nodes: number
  steps: number
  /** Of the run's steps decided to run, this reason's share (0–1). */
  share: number
  /** The first node executed for it, to link to. */
  example: string
}

export interface ReasonGroup {
  category: ReasonCategory
  label: string
  rows: ReasonRow[]
}

/** What a run says about why it executes what it does. */
export type PlanExplanation =
  /** It has decided to execute nothing (yet). */
  | { kind: 'none'; note: string }
  | { kind: 'reasons'; headline: string; groups: ReasonGroup[]; omitted: number; partial: boolean }

/**
 * A run's `plan_reasons` grouped by category, most steps first (the order the server sends).
 * `live`: the run is still deciding phases, so the reasons are the ones so far.
 */
export function planExplanation(plan: PlanReasons | null, live: boolean): PlanExplanation {
  if (plan === null || plan.steps === 0) {
    return {
      kind: 'none',
      note: live ? 'No step decided to run yet.' : 'Every step was served from cache, or the run recorded no reasons.',
    }
  }
  const groups = new Map<ReasonCategory, ReasonGroup>()
  for (const r of plan.reasons) {
    const group = groups.get(r.category) ?? { category: r.category, label: reasonLabel(r.category), rows: [] }
    group.rows.push({
      key: `${r.category}:${r.subject ?? ''}`,
      category: r.category,
      subject: subjectText(r),
      counts: `${plural(r.steps, 'step')} · ${plural(r.nodes, 'node')}`,
      nodes: r.nodes,
      steps: r.steps,
      share: Math.min(1, r.steps / plan.steps),
      example: r.example,
    })
    groups.set(r.category, group)
  }
  const headline = `${plural(plan.steps, 'step')} of ${plural(plan.nodes, 'node')} ${live ? 'decided to run so far' : 'were not cache hits'}`
  return { kind: 'reasons', headline, groups: [...groups.values()], omitted: plan.omitted, partial: live }
}

/** `5,000 new partition keys · 29 code changed (helper billing.py:_n)`: the first `parts` reasons. */
export function reasonSummary(plan: PlanReasons | null, parts = 2): string | null {
  if (!plan || plan.reasons.length === 0) return null
  return plan.reasons.slice(0, parts).map((r) => {
    const subject = subjectText(r)
    return `${r.steps.toLocaleString()} ${reasonLabel(r.category)}${subject ? ` (${subject})` : ''}`
  }).join(' · ')
}

// ── Who asked, attempts, cancellations ────────────────────────────────────────

/** `api · token:ci-bot`: what started the run and, when the server knows, who. */
export function triggerDetail(triggeredBy: TriggeredBy | null, principal: string | null): string {
  const what = triggeredBy ?? 'unknown'
  return principal ? `${what} · ${principal}` : what
}

/** `19,461 attempts · 62 retries`; null before the run recorded any. */
export function attemptsLabel(attempts: number | null, retries: number | null): string | null {
  if (attempts === null || attempts === 0) return null
  const base = plural(attempts, 'attempt')
  return retries && retries > 0 ? `${base} · ${plural(retries, 'retry', 'retries')}` : base
}

/** Why a cancelled run was cancelled, as the panel says it. `run` is a run to link to. */
export interface CancellationNotice {
  headline: string
  detail: string | null
  run: string | null
}

export function cancellationNotice(c: Cancellation | null): CancellationNotice | null {
  if (c === null) return null
  const by = c.principal ? ` by ${c.principal}` : ''
  const how = match(c.by)
    .with('request', () => `Cancelled${by}`)
    .with('shutdown', () => 'Cancelled by the server stopping')
    .exhaustive()
  if (c.started) return { headline: `${how} while it ran.`, detail: 'Steps that finished before the cancel are cached.', run: null }
  if (c.same_as !== null) {
    return {
      headline: `Not started: the same target was already running as run ${c.same_as}.`,
      detail: `${how} while it waited in the queue; it executed nothing.`,
      run: c.same_as,
    }
  }
  return {
    headline: 'Not started: cancelled while it waited in the queue.',
    detail: c.waiting_behind.length > 0 ? `It waited behind ${c.waiting_behind.map((r) => `run ${r}`).join(', ')}. ${how}.` : `${how}.`,
    run: c.waiting_behind[0] ?? null,
  }
}

// ── A run in progress ─────────────────────────────────────────────────────────

/** How far a run is: steps and the keys of the nodes it is working on. */
export interface RunProgress {
  /** Steps with a result (executed or cached) of the steps planned so far. */
  done: number
  total: number | null
  /** 0–100, null without a total. */
  percent: number | null
  executed: number
  cached: number
  /** Keys with a result of the partitioned nodes it is working on now. */
  keys: { done: number; total: number } | null
}

export interface ActiveNode {
  nodeId: string
  /** `running` or `queued`. */
  step: ActiveStep['step']
  /** `3,120 of 5,000 keys`, null for an unpartitioned node. */
  keys: string | null
  /** 0–1 of its keys done, null for an unpartitioned node. */
  share: number | null
}

export interface GroupActivity {
  group: string
  running: number
  queued: number
}

export interface SlowNode {
  nodeId: string
  /** Summed over its keys, which may have overlapped. */
  seconds: number
  keys: number
}

export interface RunOverviewModel {
  progress: RunProgress
  /** `6m 12s`. */
  elapsed: string
  /** `about 4m left`, from the rate so far; null when it cannot be told yet. */
  eta: string | null
  running: ActiveNode[]
  queued: number
  /** The groups of the nodes in flight, busiest first; empty without groups. */
  groups: GroupActivity[]
  slowest: SlowNode[]
}

/** How many nodes the overview lists as executing now, and as slowest. */
export const ACTIVE_SHOWN = 8
export const SLOWEST_SHOWN = 5

/** The innermost group of each node: the group with the fewest members that has it. */
export function groupOfNodes(groups: readonly NodeGroup[]): Map<string, string> {
  const out = new Map<string, { name: string; size: number }>()
  for (const g of groups) {
    for (const m of g.members) {
      const known = out.get(m)
      if (!known || g.members.length < known.size) out.set(m, { name: g.name, size: g.members.length })
    }
  }
  return new Map([...out].map(([node, g]) => [node, g.name]))
}

/** `about 4m left` for `seconds` to go. */
function leftLabel(seconds: number): string {
  if (seconds < 60) return 'under a minute left'
  if (seconds < 3600) return `about ${Math.round(seconds / 60)}m left`
  const h = Math.floor(seconds / 3600)
  return `about ${h}h ${Math.round((seconds % 3600) / 60)}m left`
}

/**
 * A run in progress, from `GET /runs/{id}` (`run`) and the nodes it is working on now
 * (`active`: its `active_steps` of `GET /runs?status=running`). The ETA is the steps left at the
 * rate it has executed steps so far (cached steps take no time and are left out of the rate).
 */
export function runOverview(
  run: Pick<RunView, 'started_at' | 'steps_total' | 'steps_executed' | 'steps_cached' | 'steps' | 'steps_page'>,
  active: readonly ActiveStep[],
  groups: readonly NodeGroup[],
  nowMs: number,
): RunOverviewModel {
  const done = run.steps_executed + run.steps_cached
  const total = run.steps_total
  const percent = total && total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : null
  let keysDone = 0
  let keysTotal = 0
  for (const a of active) {
    if (a.partitions) {
      keysDone += a.partitions.done
      keysTotal += a.partitions.total
    }
  }
  const started = parseTime(run.started_at)
  const elapsedS = started === null ? 0 : Math.max(0, (nowMs - started) / 1000)
  const left = total === null ? 0 : Math.max(0, total - done)
  const eta = run.steps_executed > 0 && elapsedS > 0 && left > 0
    ? leftLabel(left / (run.steps_executed / elapsedS))
    : null
  const running: ActiveNode[] = active
    .filter((a) => a.step === 'running')
    .map((a) => ({
      nodeId: a.node_id,
      step: a.step,
      keys: a.partitions ? `${a.partitions.done.toLocaleString()} of ${plural(a.partitions.total, 'key')}` : null,
      share: a.partitions && a.partitions.total > 0 ? a.partitions.done / a.partitions.total : null,
    }))
    // Furthest from done first: what holds the run up.
    .sort((x, y) => (x.share ?? 1) - (y.share ?? 1) || x.nodeId.localeCompare(y.nodeId))
  const byGroup = groupOfNodes(groups)
  const activity = new Map<string, GroupActivity>()
  for (const a of active) {
    const group = byGroup.get(a.node_id)
    if (!group) continue
    const g = activity.get(group) ?? { group, running: 0, queued: 0 }
    if (a.step === 'running') g.running += 1
    else g.queued += 1
    activity.set(group, g)
  }
  return {
    progress: {
      done, total, percent, executed: run.steps_executed, cached: run.steps_cached,
      keys: keysTotal > 0 ? { done: keysDone, total: keysTotal } : null,
    },
    elapsed: elapsedLabel(run.started_at, nowMs),
    eta,
    running,
    queued: active.filter((a) => a.step === 'queued').length,
    groups: [...activity.values()].sort((a, b) => b.running + b.queued - (a.running + a.queued) || a.group.localeCompare(b.group)),
    slowest: slowestNodes(run.steps_page.nodes ?? [], run.steps),
  }
}

/** The nodes that took longest so far: by node over every step when the server summed them, else the listed steps. */
export function slowestNodes(nodes: readonly RunNodeSummary[], steps: RunView['steps'], n = SLOWEST_SHOWN): SlowNode[] {
  const rows: SlowNode[] = nodes.length > 0
    ? nodes.map((s) => ({ nodeId: s.node_id, seconds: s.elapsed_seconds, keys: s.keys }))
    : steps.filter((s) => s.elapsed_seconds !== null).map((s) => ({ nodeId: s.node_id, seconds: s.elapsed_seconds ?? 0, keys: 1 }))
  return rows.filter((r) => r.seconds > 0).sort((a, b) => b.seconds - a.seconds || a.nodeId.localeCompare(b.nodeId)).slice(0, n)
}

// ── The steps of a big run, page by page ──────────────────────────────────────

/** What the Steps section says about the steps it lists: never a silent cap. */
export function stepsPageNote(page: RunView['steps_page'], listed: number): string | null {
  if (!page.truncated) return null
  const from = (page.offset ?? 0) + 1
  return `Showing steps ${from.toLocaleString()}–${(from + listed - 1).toLocaleString()} of ${page.total.toLocaleString()} (failed steps first).`
}

// ── A large run in flight, anywhere in the app ────────────────────────────────

/** A run of at least this many planned steps is large: worth saying app-wide while it runs. */
export const LARGE_RUN_STEPS = 1_000

export interface InFlightSummary {
  /** `running 23,278 steps · 83% · started 6m ago by token:ci-bot`. */
  label: string
  percent: number | null
  large: boolean
}

/**
 * One line about a run in flight, for a header or the health view: its size, how far it is,
 * when it started and who started it. For the queue indicator; the run panel says more.
 */
export function inFlightSummary(
  run: Pick<RunSummary, 'steps_total' | 'steps_executed' | 'steps_cached' | 'started_at' | 'triggered_by' | 'principal'>,
  nowMs: number,
): InFlightSummary {
  const total = run.steps_total
  const done = run.steps_executed + run.steps_cached
  const percent = total && total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : null
  const parts = [total !== null ? `running ${plural(total, 'step')}` : 'running']
  if (percent !== null) parts.push(`${percent}%`)
  const elapsed = elapsedLabel(run.started_at, nowMs)
  const who = run.principal ?? run.triggered_by
  parts.push(`started ${elapsed ? `${elapsed} ago` : 'just now'}${who ? ` by ${who}` : ''}`)
  return { label: parts.join(' · '), percent, large: (total ?? 0) >= LARGE_RUN_STEPS }
}
