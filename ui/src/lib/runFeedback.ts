/**
 * Inspector presentation logic — pure, exhaustively matched.
 *
 * Decides *what feedback to show* for a node given its run status, captured
 * logs, and any error. Returns a descriptor (a discriminated union) rather than
 * JSX, so the decision is testable in isolation and the component is left to do
 * nothing but map the descriptor to elements.
 */

import { match } from 'ts-pattern'
import { formatSeconds } from './assetTable'
import { parseRunStatus, runStatus } from './runs'
import type { StatusKind, LogLine, RunView } from './types'

export type RunFeedback =
  | { kind: 'idle' }
  | { kind: 'streaming'; logs: LogLine[] }
  | { kind: 'output'; logs: LogLine[] }
  | { kind: 'done' }
  | { kind: 'failed'; error: string | null; logs: LogLine[] }

/**
 * Map a node's visual status (+ logs + error) to a feedback descriptor.
 * Exhaustive over every `StatusKind`: a new status is a compile error until
 * it's given a feedback shape — that exhaustiveness is what stops a state (like
 * `failed`) from silently rendering nothing.
 */
export function runFeedback(
  status: StatusKind,
  logs: LogLine[],
  error: string | null,
): RunFeedback {
  return match(status)
    .with('running', (): RunFeedback => ({ kind: 'streaming', logs }))
    .with(
      'success',
      (): RunFeedback =>
        logs.length > 0 ? { kind: 'output', logs } : { kind: 'done' },
    )
    .with('failed', (): RunFeedback => ({ kind: 'failed', error, logs }))
    // Pre-run / inert states show no feedback panel — just the trigger button.
    .with('queued', 'pending', 'skipped', 'warning', (): RunFeedback => ({ kind: 'idle' }))
    .exhaustive()
}

// ── A run started from a page ─────────────────────────────────────────────────

/** A run this page started: the one line it follows, from the request to the outcome. */
export type RunNotice =
  | { kind: 'starting'; target: string | null }
  | { kind: 'rejected'; target: string | null; error: string }
  | { kind: 'queued'; target: string | null; runId: string }
  | { kind: 'running'; target: string | null; runId: string; done: number; total: number | null }
  | { kind: 'finished'; target: string | null; runId: string; status: StatusKind; summary: string }

export interface RunNoticeInput {
  /** What was asked for (a node id), when known. */
  target: string | null
  /** The `POST /runs` in flight. */
  pending: boolean
  /** The `POST /runs` refused (not a run that failed). */
  error: string | null
  /** The accepted run, until dismissed. */
  started: { runId: string; target: string | null } | null
  /** `GET /runs/{id}` of the accepted run, once fetched. */
  run: RunView | null
}

/** `succeeded in 2.3s`, `7 keys failed`, `failed: ValueError: …`, `cancelled`. */
export function runOutcomeSummary(run: RunView): string {
  const status = parseRunStatus(run.status)
  if (status === 'success') {
    const took = run.elapsed_seconds === null ? '' : ` in ${formatSeconds(run.elapsed_seconds)}`
    return `succeeded${took}`
  }
  if (status === 'cancelled' || status === 'interrupted') return status
  const failedKeys = run.steps_page.nodes
    ? run.steps_page.nodes.reduce((n, node) => n + node.failed, 0)
    : run.steps.filter((s) => s.status === 'failed' && s.partition_key !== null).length
  if (failedKeys > 1) return `${failedKeys.toLocaleString()} keys failed`
  const error = run.error ?? run.failure?.error ?? null
  return error ? `failed: ${error}` : 'failed'
}

/**
 * The line a page shows for the run it started, or nothing when it started none. The request
 * comes first (starting, or refused), then the accepted run as its record says: queued, running
 * with its progress, and finally its outcome. A finished run stays until dismissed.
 */
export function runNotice({ target, pending, error, started, run }: RunNoticeInput): RunNotice | null {
  if (pending) return { kind: 'starting', target }
  if (error !== null) return { kind: 'rejected', target, error }
  if (!started) return null
  const { runId } = started
  const named = started.target
  if (!run || run.run_id !== runId) return { kind: 'queued', target: named, runId }
  const status = parseRunStatus(run.status)
  if (status === 'pending' || status === null) return { kind: 'queued', target: named, runId }
  if (status === 'running') {
    return { kind: 'running', target: named, runId, done: run.steps_executed, total: run.steps_total }
  }
  return { kind: 'finished', target: named, runId, status: runStatus(run.status), summary: runOutcomeSummary(run) }
}
