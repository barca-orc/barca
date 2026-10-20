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
import { parseOutcome } from './runs'
import type { RunEvent, LineWhen, LogLine, RunOutcome, StatusKind } from './types'

/** A progress line, exactly as the CLI prints it on stderr. */
export interface ProgressLine {
  line: string
  when: LineWhen
}

export interface RunStreamState {
  /** True between run_started and run_finished. */
  running: boolean
  /** Captured log lines (what the steps printed), in order. */
  logs: LogLine[]
  /** The run's progress lines (the CLI's stderr), in order. */
  lines: ProgressLine[]
  /** Per-node visual status derived from the event stream. */
  statuses: Record<string, StatusKind>
  /** Nodes whose result was served from cache (their function did not run). */
  cached: Record<string, true>
  /** Per-node failure message, when a step finished with ok=false. */
  errors: Record<string, string>
  /** Final outcome once finished, else null. */
  ok: boolean | null
  /** How the run ended, once `run_finished` carries it. */
  outcome: RunOutcome | null
}

export const EMPTY_RUN_STATE: RunStreamState = {
  running: false,
  logs: [],
  lines: [],
  statuses: {},
  cached: {},
  errors: {},
  ok: null,
  outcome: null,
}

/**
 * Fold one event into the run state. Pure and total — every `RunEvent` variant
 * is handled via an exhaustive match, so adding a variant to the wire protocol
 * is a compile error here until it's handled.
 */
export function reduceRunEvent(state: RunStreamState, event: RunEvent): RunStreamState {
  return match(event)
    .with({ type: 'run_started' }, () => ({ ...state, running: true }))
    .with({ type: 'line' }, (ev) => ({
      ...state,
      lines: [...state.lines, { line: ev.line, when: ev.when }],
    }))
    .with({ type: 'log' }, (ev) => ({
      ...state,
      logs: [...state.logs, { nodeId: ev.node_id, text: ev.line }],
      statuses: { ...state.statuses, [ev.node_id]: 'running' as StatusKind },
    }))
    .with({ type: 'step_finished' }, (ev) => {
      // A cached step is a success whose function did not run.
      const cached = ev.ok && ev.cached === true
      return {
        ...state,
        statuses: {
          ...state.statuses,
          [ev.node_id]: (ev.ok ? 'success' : 'failed') as StatusKind,
        },
        cached: cached ? { ...state.cached, [ev.node_id]: true as const } : state.cached,
        errors:
          !ev.ok && ev.error
            ? { ...state.errors, [ev.node_id]: ev.error }
            : state.errors,
      }
    })
    .with({ type: 'run_finished' }, (ev) => ({
      ...state,
      running: false,
      ok: ev.ok,
      outcome: parseOutcome(ev.outcome),
    }))
    .exhaustive()
}

/**
 * The stream ended without a `run_finished` (the server no longer has the run
 * live, or the connection failed for good). The run is no longer watched:
 * stop showing it as running; its result comes from `GET /runs/{run_id}`.
 */
export function endRunStream(state: RunStreamState): RunStreamState {
  return state.running ? { ...state, running: false } : state
}

/**
 * Whether an SSE frame with this `id:` is new. Frames are numbered from 0; a
 * reconnect replays the run from its first event, so frames already folded in
 * are skipped. A frame without an id is always applied.
 */
export function isNewFrame(lastId: number | null, frameId: string): boolean {
  if (frameId === '') return true
  const id = Number(frameId)
  if (!Number.isInteger(id)) return true
  return lastId === null || id > lastId
}
