/**
 * The Failures view as a program: failures grouped by cause, and what a row can do about one.
 * The list is exactly what `GET /state` reports failed in scope: nothing is hidden on the client.
 */

import { match } from 'ts-pattern'
import { failureGroups, type Failure, type FailureGroup } from './health'
import { only, type Next } from './program'
import type { NodeKind } from './types'

export interface FailuresModel {
  /** The last action's outcome. */
  notice: { text: string; failed: boolean } | null
}

export const INITIAL_FAILURES: FailuresModel = { notice: null }

export type FailuresMsg =
  | { type: 'retry'; id: string; kind: NodeKind; name: string }
  | { type: 'copy'; error: string }
  | { type: 'open_run'; id: string; name: string; runId: string | null }
  | { type: 'done'; text: string }
  | { type: 'action_failed'; text: string }

export type FailuresEffect =
  | { type: 'retry'; id: string; kind: NodeKind; name: string }
  | { type: 'copy'; text: string }
  | { type: 'open_run'; id: string; name: string; runId: string | null }

export function updateFailures(model: FailuresModel, msg: FailuresMsg): Next<FailuresModel, FailuresEffect> {
  return match(msg)
    .returnType<Next<FailuresModel, FailuresEffect>>()
    .with({ type: 'retry' }, ({ id, kind, name }) => [{ ...model, notice: null }, [{ type: 'retry', id, kind, name }]])
    .with({ type: 'copy' }, ({ error }) => [{ ...model, notice: null }, [{ type: 'copy', text: error }]])
    .with({ type: 'open_run' }, ({ id, name, runId }) => [{ ...model, notice: null }, [{ type: 'open_run', id, name, runId }]])
    .with({ type: 'done' }, ({ text }) => only({ ...model, notice: { text, failed: false } }))
    .with({ type: 'action_failed' }, ({ text }) => only({ ...model, notice: { text, failed: true } }))
    .exhaustive()
}

export type FailureRow =
  | { kind: 'cause'; group: FailureGroup }
  | { kind: 'failure'; failure: Failure }

export interface FailureTable {
  rows: FailureRow[]
}

/** The table's rows, one heading per cause. */
export function failureTable(list: readonly Failure[]): FailureTable {
  return {
    rows: failureGroups(list).flatMap((group): FailureRow[] => [
      { kind: 'cause', group },
      ...group.failures.map((failure): FailureRow => ({ kind: 'failure', failure })),
    ]),
  }
}
