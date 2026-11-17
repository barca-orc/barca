/**
 * The Failures view as a program: the failure roots of the rollup under the error each shares,
 * and what a row can do about one. The list is exactly the roots `GET /state/rollup` names in
 * scope, in its order: nothing is hidden, counted or regrouped on the client.
 */

import { match } from 'ts-pattern'
import type { Failure } from './health'
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
  /** A heading: the error the failures under it share, and how many rows follow it. */
  | { kind: 'cause'; key: string; headline: string; failed: number }
  /**
   * `sameError`: the failure's headline is its cause heading's, word for word, so its row does
   * not repeat it (the heading says it once; the row keeps the copy action and its keys line).
   */
  | { kind: 'failure'; failure: Failure; sameError: boolean }

interface FailureTable {
  rows: FailureRow[]
}

/**
 * The table's rows: `list` as served (the failure holding the most nodes first), each under a
 * heading for the error group the server gave it (`error_group`); a heading stands where its
 * first failure does. A failure with no group is under a heading of its own.
 */
export function failureTable(list: readonly Failure[]): FailureTable {
  const groups = new Map<string, Failure[]>()
  for (const f of list) {
    const key = f.group ?? `node:${f.id}`
    const members = groups.get(key)
    if (members) members.push(f)
    else groups.set(key, [f])
  }
  return {
    rows: [...groups].flatMap(([key, members]): FailureRow[] => {
      const headline = members[0]!.headline
      return [
        { kind: 'cause', key, headline, failed: members.length },
        ...members.map((failure): FailureRow => ({ kind: 'failure', failure, sameError: failure.headline === headline })),
      ]
    }),
  }
}
