/**
 * Inspector presentation logic — pure, exhaustively matched.
 *
 * Decides *what feedback to show* for a node given its run status, captured
 * logs, and any error. Returns a descriptor (a discriminated union) rather than
 * JSX, so the decision is testable in isolation and the component is left to do
 * nothing but map the descriptor to elements.
 */

import { match } from 'ts-pattern'
import type { StatusKind, LogLine } from './types'

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
