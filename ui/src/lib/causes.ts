/**
 * Causes, not rows: what is wrong in a scope, one entry per root cause, the biggest first.
 * A failure blocks the nodes below it, so the 58 nodes waiting on one failing check are one
 * entry ("58 blocked"), and failures that share an error (ignoring its numbers) are one cause.
 * Data signals and lateness are causes too: each names the node and what is off.
 */

import { causeOf, headline, type Failure } from './health'
import type { HealthTree } from './healthTree'

export type Cause =
  | {
      kind: 'failure'
      /** The shared error headline, as the first failure recorded it. */
      headline: string
      /** The failing nodes with this error, the most blocking first. */
      nodes: string[]
      /** Failed keys of partitioned failing nodes (0 when none are partitioned). */
      failedKeys: number
      /** Nodes kept from running by these failures. */
      blocked: number
      /** The newest failure's time and the longest streak, for the line under the headline. */
      at: string | null
      failingSince: string | null
      runs: number
    }
  | { kind: 'suspect'; node: string; detail: string }
  | { kind: 'behind'; node: string; detail: string }

/** How big a cause is: what the list orders by. */
export function causeWeight(cause: Cause): number {
  if (cause.kind === 'failure') return cause.blocked + Math.max(cause.nodes.length, cause.failedKeys)
  return 1
}

/** Nodes each failing node keeps from running, from the tree's blocked attribution. */
function blockedCounts(tree: HealthTree): Map<string, number> {
  const out = new Map<string, number>()
  for (const by of tree.blockedBy.values()) out.set(by, (out.get(by) ?? 0) + 1)
  return out
}

export interface CausesInput {
  tree: HealthTree
  /** The failures in scope (`health.failures`), with key errors where loaded. */
  failures: readonly Failure[]
  /** Data signals and lateness, already worded, by node. */
  suspect?: ReadonlyMap<string, string>
  behind?: ReadonlyMap<string, string>
  /** Only causes about these nodes (a scope); every node when absent. */
  within?: ReadonlySet<string>
}

/** The causes in scope, the biggest first. */
export function causes({ tree, failures, suspect, behind, within }: CausesInput): Cause[] {
  const inScope = (id: string) => !within || within.has(id)
  const blocked = blockedCounts(tree)
  const groups = new Map<string, Extract<Cause, { kind: 'failure' }>>()
  for (const f of failures) {
    if (!inScope(f.id)) continue
    const line = f.headline || headline(f.error)
    const key = causeOf(line)
    const group = groups.get(key) ?? { kind: 'failure', headline: line, nodes: [], failedKeys: 0, blocked: 0, at: null, failingSince: null, runs: 0 }
    group.nodes.push(f.id)
    group.failedKeys += f.keys?.failed ?? 0
    group.blocked += blocked.get(f.id) ?? 0
    if (f.at && (!group.at || f.at > group.at)) group.at = f.at
    if (f.failingSince && (!group.failingSince || f.failingSince < group.failingSince)) group.failingSince = f.failingSince
    group.runs = Math.max(group.runs, f.failures)
    groups.set(key, group)
  }
  for (const g of groups.values()) g.nodes.sort((a, b) => (blocked.get(b) ?? 0) - (blocked.get(a) ?? 0) || a.localeCompare(b))
  const out: Cause[] = [...groups.values()]
  for (const [node, detail] of suspect ?? []) if (inScope(node)) out.push({ kind: 'suspect', node, detail })
  for (const [node, detail] of behind ?? []) if (inScope(node)) out.push({ kind: 'behind', node, detail })
  const rank = (c: Cause) => (c.kind === 'failure' ? 0 : c.kind === 'suspect' ? 1 : 2)
  return out.sort((a, b) => rank(a) - rank(b) || causeWeight(b) - causeWeight(a))
}

/** `1,205 keys · 150 blocked`: a failure cause's size, in a line. */
export function causeImpact(cause: Extract<Cause, { kind: 'failure' }>): string {
  const parts: string[] = []
  if (cause.failedKeys > 0) parts.push(`${cause.failedKeys.toLocaleString()} ${cause.failedKeys === 1 ? 'key' : 'keys'} failed`)
  else if (cause.nodes.length > 1) parts.push(`${cause.nodes.length} nodes`)
  if (cause.blocked > 0) parts.push(`${cause.blocked.toLocaleString()} blocked downstream`)
  return parts.join(' · ')
}
