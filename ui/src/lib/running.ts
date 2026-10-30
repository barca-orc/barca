/**
 * What is running now, from `GET /state`'s `running` field — whoever started the run
 * (the scheduler, another client, the CLI). Pure: the pages and hooks only map these
 * descriptors to elements and poll intervals.
 */

import type { GraphState } from './graphState'
import type { NodeRunning, NodeState, StatusKind, TriggeredBy } from './types'

/** `/state` poll interval while some node is running, and otherwise. */
export const POLL_RUNNING_MS = 2_000
export const POLL_IDLE_MS = 10_000
/** The sidebar's whole-project state: seconds of server work on a big project, so rarely. */
export const POLL_WHOLE_MS = 60_000

/** How often to poll `/state`: faster while anything runs. */
export function statePollInterval(nodes: NodeState[] | undefined): number {
  return nodes?.some((n) => n.running) ? POLL_RUNNING_MS : POLL_IDLE_MS
}

/** The visual status of a node a run is working on. */
export function runningStatus(running: NodeRunning): StatusKind {
  return running.step === 'running' ? 'running' : 'queued'
}

/** `3/10 keys` for a partitioned node, else null. */
export function keysLabel(running: NodeRunning): string | null {
  const p = running.partitions
  return p ? `${p.done}/${p.total} keys` : null
}

const STARTED_BY: Record<TriggeredBy, string> = {
  schedule: 'the scheduler',
  api: 'the API',
  client: 'a client',
  manual: 'the CLI',
}

/** Who started the run, in words. */
export function startedBy(triggered: TriggeredBy | null): string {
  return triggered ? STARTED_BY[triggered] : 'another process'
}

/** Elapsed time since `startedAt` (RFC 3339) as `45s`, `3m 12s` or `2h 5m`. */
export function elapsedLabel(startedAt: string, nowMs: number): string {
  const started = Date.parse(startedAt)
  if (Number.isNaN(started)) return ''
  const s = Math.max(0, Math.floor((nowMs - started) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

/** The graph/table state of a running node: it pulses (running) or waits (queued). */
export function runningGraphState(running: NodeRunning): GraphState {
  const keys = keysLabel(running)
  const word = running.step === 'running' ? 'Running' : 'Running · queued'
  return {
    status: runningStatus(running),
    label: keys ? `${word} · ${keys}` : word,
    hint: `Run ${running.run_id}, started by ${startedBy(running.triggered_by)}`,
  }
}

/** The Running badge of the assets list: label with elapsed time, and the run's link. */
export interface RunningBadge {
  status: StatusKind
  label: string
  hint: string
  href: string
}

export function runningBadge(running: NodeRunning, nowMs: number): RunningBadge {
  const state = runningGraphState(running)
  const elapsed = elapsedLabel(running.started_at, nowMs)
  return {
    status: state.status,
    label: elapsed ? `${state.label} · ${elapsed}` : state.label,
    hint: state.hint,
    href: runHref(running.run_id),
  }
}

/** The Runs page with this run open. */
export function runHref(runId: string): string {
  return `/runs?run=${encodeURIComponent(runId)}`
}

/** How many of `ids` a run is working on now. */
export function runningCount(ids: string[], byId: Map<string, NodeState>, live: Record<string, StatusKind> = {}): number {
  return ids.filter((id) => byId.get(id)?.running || live[id] === 'running' || live[id] === 'queued').length
}

/** The distinct runs in progress, by the nodes they are working on. */
export function runsInProgress(nodes: NodeState[]): string[] {
  return [...new Set(nodes.flatMap((n) => (n.running ? [n.running.run_id] : [])))]
}

/** `1 run in progress` / `3 runs in progress`. */
export function runsInProgressLabel(count: number): string {
  return `${count} run${count === 1 ? '' : 's'} in progress`
}
