/**
 * Run-stream presentation logic — pure, framework-free, exhaustively matched.
 *
 * This module owns the *decision* of how a sequence of `RunEvent`s folds into
 * the state the UI renders. It has no React, no EventSource, no I/O — so it can
 * be unit-tested directly by feeding it events and asserting the result. The
 * `useRunStream` hook is the thin presentation wrapper that pipes live SSE
 * events through `reduceRunEvent`.
 */

import { match } from 'ts-pattern'
import type { RunEvent, LogLine, StatusKind } from './types'

export interface RunStreamState {
  /** True between run_started and run_finished. */
  running: boolean
  /** Captured log lines, in order. */
  logs: LogLine[]
  /** Per-node visual status derived from the event stream. */
  statuses: Record<string, StatusKind>
  /** Per-node failure message, when a step finished with ok=false. */
  errors: Record<string, string>
  /** Final outcome once finished, else null. */
  ok: boolean | null
}

export const EMPTY_RUN_STATE: RunStreamState = {
  running: false,
  logs: [],
  statuses: {},
  errors: {},
  ok: null,
}

/**
 * Fold one event into the run state. Pure and total — every `RunEvent` variant
 * is handled via an exhaustive match, so adding a variant to the wire protocol
 * is a compile error here until it's handled.
 */
export function reduceRunEvent(state: RunStreamState, event: RunEvent): RunStreamState {
  return match(event)
    .with({ type: 'run_started' }, () => ({ ...state, running: true }))
    .with({ type: 'log' }, (ev) => ({
      ...state,
      logs: [...state.logs, { nodeId: ev.node_id, text: ev.line }],
      statuses: { ...state.statuses, [ev.node_id]: 'running' as StatusKind },
    }))
    .with({ type: 'step_finished' }, (ev) => ({
      ...state,
      statuses: {
        ...state.statuses,
        [ev.node_id]: (ev.ok ? 'success' : 'failed') as StatusKind,
      },
      errors:
        !ev.ok && ev.error
          ? { ...state.errors, [ev.node_id]: ev.error }
          : state.errors,
    }))
    .with({ type: 'run_finished' }, (ev) => ({
      ...state,
      running: false,
      ok: ev.ok,
    }))
    .exhaustive()
}
