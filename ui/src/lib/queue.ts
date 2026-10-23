/**
 * The run queue (`GET /queue`) as the Runs page shows it: how full it is, whether it is paused
 * or draining, what the operator may do about it, and the waiting runs in order. Pure: the
 * panel and its hook only map these to elements and poll intervals.
 */

import { match } from 'ts-pattern'
import { runHref } from './running'
import { runTime, triggeredByLabel } from './runs'
import type { CapacitySnapshot, HealthQueue, QueueEntry, QueueView } from './types'

/** `/queue` poll interval while anything runs or waits, and otherwise. */
export const QUEUE_POLL_BUSY_MS = 2_000
export const QUEUE_POLL_IDLE_MS = 10_000

export function queuePollInterval(view: QueueView | undefined): number {
  return view && (view.running.length > 0 || view.queued.length > 0) ? QUEUE_POLL_BUSY_MS : QUEUE_POLL_IDLE_MS
}

/** Whether new runs start. Draining wins over paused: nothing starts until the next server start. */
export type QueueState =
  | { kind: 'open' }
  | { kind: 'paused'; since: string | null }
  | { kind: 'draining' }

export function queueState(view: QueueView): QueueState {
  if (view.draining) return { kind: 'draining' }
  if (view.paused) return { kind: 'paused', since: view.paused_at }
  return { kind: 'open' }
}

/** The state in words; null while the queue is open. */
export function queueStateLabel(state: QueueState, formatTime: (value: string | null) => string = runTime): string | null {
  return match(state)
    .with({ kind: 'open' }, () => null)
    .with({ kind: 'paused' }, (s) => (s.since ? `paused since ${formatTime(s.since)}` : 'paused'))
    .with({ kind: 'draining' }, () => 'draining (server restarting)')
    .exhaustive()
}

/** `1 of 4 running`. */
export function capacityLabel(running: number, maxRuns: number): string {
  return `${running} of ${maxRuns} running`
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

/** One step waiting for the shared capacity, in admission order. */
export interface WaitingRow {
  position: string
  node: string
  runId: string
  weight: number
  waited: string
  href: string
}

export function waitingRows(capacity: CapacitySnapshot | null | undefined): WaitingRow[] {
  return (capacity?.waiting ?? []).map((w, at) => ({
    position: `#${at + 1}`,
    node: w.node,
    runId: w.run_id,
    weight: w.weight,
    waited: `${Math.round(w.waiting_seconds)}s`,
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
export type QueueControl =
  | { kind: 'hidden' }
  | { kind: 'pause'; disabled: boolean }
  | { kind: 'resume'; disabled: boolean }

export function queueControl(view: QueueView, readOnly: boolean | undefined): QueueControl {
  if (readOnly !== false) return { kind: 'hidden' }
  return view.paused ? { kind: 'resume', disabled: view.draining } : { kind: 'pause', disabled: view.draining }
}

/** One waiting run, as a row of the queue list. */
export interface QueueRow {
  runId: string
  /** `#1` for the next run. */
  position: string
  target: string
  command: string
  trigger: string
  /** The schedule that queued it, for a scheduled run. */
  scheduleId: string | null
  queuedAt: string
  href: string
}

export function queueRows(entries: QueueEntry[], formatTime: (value: string | null) => string = runTime): QueueRow[] {
  return [...entries]
    .sort((a, b) => (a.position ?? Infinity) - (b.position ?? Infinity))
    .map((entry) => ({
      runId: entry.run_id,
      position: entry.position === null ? '–' : `#${entry.position}`,
      target: entry.target ?? 'all assets',
      command: entry.command,
      trigger: triggeredByLabel(entry.triggered_by),
      scheduleId: entry.schedule_id,
      queuedAt: formatTime(entry.queued_at),
      href: runHref(entry.run_id),
    }))
}

/** The header chip's note on the queue from `/health`: `3 queued`, `3 queued · paused`; null when empty and open. */
export function healthQueueLabel(queue: HealthQueue | undefined): string | null {
  if (!queue) return null
  if (queue.queued === 0) return queue.paused ? 'queue paused' : null
  return queue.paused ? `${queue.queued} queued · paused` : `${queue.queued} queued`
}
