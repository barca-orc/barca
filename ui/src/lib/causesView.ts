/**
 * Causes, not rows: what is wrong in a scope, one entry per root, as the rollup serves them
 * (`roots` of `GET /state/rollup`: a failing node with the nodes it holds, the most nodes
 * first). Failures that share an error (the server's `error_group`) are one entry, headed by
 * the first of them, with its own numbers and how many more share the error; nothing is added
 * up here. The roots that are not failures follow as served (a changed sensor value or code, a
 * missing or paused schedule, a run queue that is behind), each with what it holds and the
 * action that clears it. Data signals and lateness follow, each naming the node and what is off.
 */

import type { Failure } from './health'
import { plural } from './plural'
import type { ActionKind } from './generated/ActionKind'
import type { RollupRoot } from './generated/RollupRoot'
import type { RootKind } from './generated/RootKind'

/** A root kind's display word: the one `barca status --rollup` prints. */
export const ROOT_KIND_WORD: Record<RootKind, string> = {
  sensor_value: 'sensor value',
  code: 'code change',
  env: 'environment change',
  input: 'input change',
  never_run: 'never run',
  no_schedule: 'no schedule',
  schedule_paused: 'paused',
  scheduler_off: 'scheduler off',
  failure: 'failure',
  queue: 'run queue',
  undecided: 'undecided',
}

/** An action kind's display word: the one `barca status --rollup` prints. */
export const ACTION_WORD: Record<ActionKind, string> = {
  wait: 'wait',
  run: 'run',
  add_schedule: 'add a schedule',
  resume_schedule: 'resume',
  start_server: 'start a server',
  fix_failure: 'fix the failure',
  raise_max_runs: 'raise max_runs',
}

export type Cause =
  | {
      kind: 'failure'
      /** The shared error headline, as the first failure recorded it. */
      headline: string
      /** The failure the entry names and opens: the one holding the most nodes. */
      node: string
      /** How many more failures share the error. */
      others: number
      /** Failed keys of that node (0 when it is not partitioned). */
      failedKeys: number
      /** Nodes that node holds, itself included, and how many of them are assets. */
      held: number
      heldAssets: number
      failingSince: string | null
    }
  | {
      /** A root that is not a failure, as served: what it is, what it holds, what clears it. */
      kind: 'held'
      node: string
      /** The root's kind in words, and the fact in a line (`detail`). */
      what: string
      detail: string
      held: number
      heldAssets: number
      /** The action in words, and its command when it has one. */
      action: string
      command: string | null
    }
  | { kind: 'suspect'; node: string; detail: string }
  | { kind: 'behind'; node: string; detail: string }

export interface CausesInput {
  /** The failures in scope (`health.failures`), in served order. */
  failures: readonly Failure[]
  /** The rollup's roots in scope, in served order: those that are not failures are listed as served. */
  roots?: readonly RollupRoot[]
  /** Data signals and lateness, already worded, by node. */
  suspect?: ReadonlyMap<string, string>
  behind?: ReadonlyMap<string, string>
  /** Only signals about these nodes (a scope); every node when absent. */
  within?: ReadonlySet<string>
}

/** The causes in scope: failures as served, the other roots as served, then suspect data, then what is behind. */
export function causes({ failures, roots, suspect, behind, within }: CausesInput): Cause[] {
  const inScope = (id: string) => !within || within.has(id)
  const groups = new Map<string, Extract<Cause, { kind: 'failure' }>>()
  for (const f of failures) {
    const key = f.group ?? `node:${f.id}`
    const known = groups.get(key)
    if (known) known.others += 1
    else groups.set(key, { kind: 'failure', headline: f.headline, node: f.id, others: 0, failedKeys: f.keys?.failed ?? 0, held: f.held, heldAssets: f.heldAssets, failingSince: f.failingSince })
  }
  const out: Cause[] = [...groups.values()]
  for (const root of roots ?? []) {
    if (root.kind === 'failure') continue
    out.push({ kind: 'held', node: root.node, what: ROOT_KIND_WORD[root.kind], detail: root.detail, held: root.nodes, heldAssets: root.assets, action: ACTION_WORD[root.action.kind], command: root.action.command })
  }
  for (const [node, detail] of suspect ?? []) if (inScope(node)) out.push({ kind: 'suspect', node, detail })
  for (const [node, detail] of behind ?? []) if (inScope(node)) out.push({ kind: 'behind', node, detail })
  return out
}

/** `holds 150 nodes (140 assets)`: what a root that is not a failure holds, each count with its unit. */
export function heldImpact(cause: Extract<Cause, { kind: 'held' }>): string {
  return `holds ${plural(cause.held, 'node')} (${plural(cause.heldAssets, 'asset')})`
}

/** `1,205 keys failed · holds 150 nodes (140 assets)`: a failure's size, each count with its unit. */
export function causeImpact(cause: Extract<Cause, { kind: 'failure' }>): string {
  const parts: string[] = []
  if (cause.failedKeys > 0) parts.push(`${cause.failedKeys.toLocaleString()} ${cause.failedKeys === 1 ? 'key' : 'keys'} failed`)
  if (cause.held > 1) parts.push(`holds ${plural(cause.held, 'node')} (${plural(cause.heldAssets, 'asset')})`)
  return parts.join(' · ')
}
