/**
 * Causes, not rows: what is wrong in a scope, one entry per root, as the rollup serves them
 * (`roots` of `GET /state/rollup`: a failing node with the nodes it holds, the most nodes
 * first). Failures that share an error (the server's `error_group`) are one entry, headed by
 * the first of them, with its own numbers and how many more share the error; nothing is added
 * up here. Data signals and lateness follow, each naming the node and what is off.
 */

import type { Failure } from './health'
import { plural } from './plural'

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
  | { kind: 'suspect'; node: string; detail: string }
  | { kind: 'behind'; node: string; detail: string }

export interface CausesInput {
  /** The failures in scope (`health.failures`), in served order. */
  failures: readonly Failure[]
  /** Data signals and lateness, already worded, by node. */
  suspect?: ReadonlyMap<string, string>
  behind?: ReadonlyMap<string, string>
  /** Only signals about these nodes (a scope); every node when absent. */
  within?: ReadonlySet<string>
}

/** The causes in scope: failures as served, then suspect data, then what is behind. */
export function causes({ failures, suspect, behind, within }: CausesInput): Cause[] {
  const inScope = (id: string) => !within || within.has(id)
  const groups = new Map<string, Extract<Cause, { kind: 'failure' }>>()
  for (const f of failures) {
    const key = f.group ?? `node:${f.id}`
    const known = groups.get(key)
    if (known) known.others += 1
    else groups.set(key, { kind: 'failure', headline: f.headline, node: f.id, others: 0, failedKeys: f.keys?.failed ?? 0, held: f.held, heldAssets: f.heldAssets, failingSince: f.failingSince })
  }
  const out: Cause[] = [...groups.values()]
  for (const [node, detail] of suspect ?? []) if (inScope(node)) out.push({ kind: 'suspect', node, detail })
  for (const [node, detail] of behind ?? []) if (inScope(node)) out.push({ kind: 'behind', node, detail })
  return out
}

/** `1,205 keys failed · holds 150 nodes (140 assets)`: a failure's size, each count with its unit. */
export function causeImpact(cause: Extract<Cause, { kind: 'failure' }>): string {
  const parts: string[] = []
  if (cause.failedKeys > 0) parts.push(`${cause.failedKeys.toLocaleString()} ${cause.failedKeys === 1 ? 'key' : 'keys'} failed`)
  if (cause.held > 1) parts.push(`holds ${plural(cause.held, 'node')} (${plural(cause.heldAssets, 'asset')})`)
  return parts.join(' · ')
}
