/**
 * The run queue (`GET /queue`) as the Runs page shows it: how full it is, whether it is paused
 * or draining, what the operator may do about it, and the waiting runs in order. Pure: the
 * panel and its hook only map these to elements and poll intervals.
 */

import { match } from 'ts-pattern'
import { runHref } from './running'
import { triggeredByLabel } from './runs'
import { formatAbout, parseTime } from './time'
import type { CapacitySnapshot, HealthQueue, QueueEntry, QueueView } from './types'

/** `/queue` poll interval while anything runs or waits, and otherwise. */
export const QUEUE_POLL_BUSY_MS = 2_000
export const QUEUE_POLL_IDLE_MS = 10_000

export function queuePollInterval(view: QueueView | undefined): number {
  return view && (view.running.length > 0 || view.queued.length > 0) ? QUEUE_POLL_BUSY_MS : QUEUE_POLL_IDLE_MS
}

/** Whether the queue has anything to say: work, or a state that holds work back. */
export function queueMatters(view: QueueView): boolean {
  return view.running.length > 0 || view.queued.length > 0 || view.paused || view.draining || waitingRows(view.capacity).length > 0
}

/** Whether new runs start. Draining wins over paused: nothing starts until the next server start. */
type QueueState =
  | { kind: 'open' }
  | { kind: 'paused'; since: string | null }
  | { kind: 'draining' }

export function queueState(view: QueueView): QueueState {
  if (view.draining) return { kind: 'draining' }
  if (view.paused) return { kind: 'paused', since: view.paused_at }
  return { kind: 'open' }
}

/** The state in words; null while the queue is open. */
export function queueStateLabel(state: QueueState, formatTime: (value: string | null) => string): string | null {
  return match(state)
    .with({ kind: 'open' }, () => null)
    .with({ kind: 'paused' }, (s) => (s.since ? `paused since ${formatTime(s.since)}` : 'paused'))
    .with({ kind: 'draining' }, () => 'draining (server restarting)')
    .exhaustive()
}

/**
 * The capacity every run shares (`--capacity`): `weight 3 of 4 in use`, with how many steps
 * wait for it; null when the server has none.
 */
export function sharedCapacityLabel(capacity: CapacitySnapshot | null | undefined): string | null {
  if (!capacity) return null
  const used = `weight ${capacity.in_use} of ${capacity.total} in use`
  const n = capacity.waiting.length
  return n === 0 ? used : `${used} · ${n} step${n === 1 ? '' : 's'} waiting`
}

/**
 * How the shared capacity orders the steps that wait (`[server] admission`), for the capacity's
 * tooltip: `requested runs first; a scheduled step waits at most 120s`.
 */
export function admissionLabel(capacity: CapacitySnapshot): string {
  return capacity.admission === 'first_come'
    ? 'first come, first served'
    : `requested runs first; a scheduled step waits at most ${capacity.scheduled_max_wait_seconds ?? '?'}s`
}

/** One step waiting for the shared capacity, in admission order. */
interface WaitingRow {
  position: string
  node: string
  runId: string
  weight: number
  waited: string
  /** `requested` or `scheduled`: what puts it ahead in the line. */
  origin: string
  href: string
}

export function waitingRows(capacity: CapacitySnapshot | null | undefined): WaitingRow[] {
  return (capacity?.waiting ?? []).map((w, at) => ({
    position: `#${at + 1}`,
    node: w.node,
    runId: w.run_id,
    weight: w.weight,
    waited: `${Math.round(w.waiting_seconds)}s`,
    origin: w.origin,
    href: runHref(w.run_id),
  }))
}

/** `3 queued`, or `nothing queued`. */
export function queuedLabel(count: number): string {
  return count === 0 ? 'nothing queued' : `${count} queued`
}

/**
 * The pause control. Hidden on a read-only server (pause and resume answer 403 there) and
 * while it is not known whether the server is read-only; disabled while the server drains.
 */
type QueueControl =
  | { kind: 'hidden' }
  | { kind: 'pause'; disabled: boolean }
  | { kind: 'resume'; disabled: boolean }

export function queueControl(view: QueueView, readOnly: boolean | undefined): QueueControl {
  if (readOnly !== false) return { kind: 'hidden' }
  return view.paused ? { kind: 'resume', disabled: view.draining } : { kind: 'pause', disabled: view.draining }
}

/** `45s`, `20m`, `2h 5m`, `3d 4h`: how long something has run or waited, in its two largest units. */
export function waitLabel(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`
  const d = Math.floor(h / 24)
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`
}

/** Seconds from `value` to `nowMs`; null when `value` is not a time. */
function secondsSince(value: string, nowMs: number): number | null {
  const at = parseTime(value)
  return at === null ? null : Math.max(0, (nowMs - at) / 1000)
}

/** `waiting_for` of a queued run in words; null when nothing but the runs ahead hold it. */
export function waitingForLabel(waitingFor: QueueEntry['waiting_for']): string | null {
  return match(waitingFor)
    .with('restart', () => 'after restart')
    .with(null, () => null)
    .exhaustive()
}

/** One run of the queue list: a running one (first) or a waiting one, in order. */
interface QueueRow {
  runId: string
  /** `running` for a run that executes now, `#1` for the next run to start. */
  state: 'running' | 'waiting'
  position: string
  target: string
  command: string
  trigger: string
  /** The schedule that queued it, for a scheduled run. */
  scheduleId: string | null
  queuedAt: string
  /** How long it has run (`running`) or waited (`waiting`): `20m`; `–` when not known. */
  elapsed: string
  /**
   * For a waiting run, when the server expects it to start (`estimated_start_at`), as an
   * estimate: `starts in ~40s`; `no estimate` when the server gives none (the queue is paused
   * or draining, or the server only reads). Null for a running run.
   */
  starts: string | null
  /** What else it waits for, in words: `after restart` while the server drains; null otherwise. */
  waitingFor: string | null
  href: string
}

/** `starts in ~40s`: a waiting run's estimated start, from now; `starting` once the time has come. */
export function startsLabel(estimatedStartAt: string | null, nowMs: number): string {
  const at = estimatedStartAt === null ? null : parseTime(estimatedStartAt)
  if (at === null) return 'no estimate'
  const left = (at - nowMs) / 1000
  return left <= 0 ? 'starting' : `starts in ${formatAbout(left)}`
}

/**
 * The queue list: the running runs first (oldest first), then the waiting ones, next first.
 * A running run's time is from its start when the run list knows it (`startedAt`), else from
 * when the server accepted it.
 */
export function queueRows(
  view: Pick<QueueView, 'running' | 'queued'>,
  formatTime: (value: string | null) => string,
  nowMs: number,
  startedAt: (runId: string) => string | null = () => null,
): QueueRow[] {
  const row = (entry: QueueEntry, state: QueueRow['state'], since: string): QueueRow => {
    const seconds = secondsSince(since, nowMs)
    return {
      runId: entry.run_id,
      state,
      position: state === 'running' ? 'running' : entry.position === null ? '–' : `#${entry.position}`,
      target: entry.target_line,
      command: entry.command,
      trigger: triggeredByLabel(entry.triggered_by),
      scheduleId: entry.schedule_id,
      queuedAt: formatTime(entry.queued_at),
      elapsed: seconds === null ? '–' : waitLabel(seconds),
      starts: state === 'running' ? null : startsLabel(entry.estimated_start_at, nowMs),
      waitingFor: waitingForLabel(entry.waiting_for),
      href: runHref(entry.run_id),
    }
  }
  const running = [...view.running]
    .sort((a, b) => a.queued_at.localeCompare(b.queued_at))
    .map((entry) => row(entry, 'running', startedAt(entry.run_id) ?? entry.queued_at))
  const waiting = [...view.queued]
    .sort((a, b) => (a.position ?? Infinity) - (b.position ?? Infinity))
    .map((entry) => row(entry, 'waiting', entry.queued_at))
  return [...running, ...waiting]
}

/** The queue in brief, for the line every page shows while runs wait. */
export interface QueueStatus {
  running: number
  waiting: number
  /** How long the run waiting longest has waited, seconds; null with none waiting. */
  oldestWaitSeconds: number | null
  paused: boolean
  draining: boolean
}

/**
 * The queue in brief while there is a queue: runs waiting, or a queue that holds work back
 * (paused, draining). Null otherwise: a run that executes with nothing behind it is no queue.
 */
export function queueStatus(view: QueueView, nowMs: number): QueueStatus | null {
  if (view.queued.length === 0 && !view.paused && !view.draining) return null
  const waits = view.queued.flatMap((entry) => {
    const s = secondsSince(entry.queued_at, nowMs)
    return s === null ? [] : [s]
  })
  return {
    running: view.running.length,
    waiting: view.queued.length,
    oldestWaitSeconds: waits.length ? Math.max(...waits) : null,
    paused: view.paused,
    draining: view.draining,
  }
}

/** `1 running · 18 waiting · oldest 20m`, then `paused` or `draining` when it holds work back. */
export function queueStatusLabel(status: QueueStatus): string {
  const parts = [`${status.running} running`, `${status.waiting} waiting`]
  if (status.oldestWaitSeconds !== null) parts.push(`oldest ${waitLabel(status.oldestWaitSeconds)}`)
  if (status.draining) parts.push('draining')
  else if (status.paused) parts.push('paused')
  return parts.join(' · ')
}

/**
 * The Queue section's line when its list is collapsed: how long the oldest has waited and
 * what starts next. Null when nothing waits.
 */
export function queueSummary(view: QueueView, nowMs: number): string | null {
  const status = queueStatus(view, nowMs)
  if (!status || status.waiting === 0) return null
  const next = [...view.queued].sort((a, b) => (a.position ?? Infinity) - (b.position ?? Infinity))[0]
  const parts: string[] = []
  if (status.oldestWaitSeconds !== null) parts.push(`oldest waiting ${waitLabel(status.oldestWaitSeconds)}`)
  if (next) parts.push(`next ${next.target_line}`)
  if (next && next.estimated_start_at !== null) parts.push(startsLabel(next.estimated_start_at, nowMs))
  return parts.join(' · ') || null
}

/** The header chip's note on the queue from `/health`: `3 queued`, `3 queued · paused`; null when empty and open. */
export function healthQueueLabel(queue: HealthQueue | undefined): string | null {
  if (!queue) return null
  if (queue.queued === 0) return queue.paused ? 'queue paused' : null
  return queue.paused ? `${queue.queued} queued · paused` : `${queue.queued} queued`
}
