/**
 * When a node or a scope will be fresh, in the words a row shows. Every time and count is the
 * server's (`expected_fresh_at`, `recheck_at`, `no_estimate` of a row of `GET /state`;
 * `fresh_by`, `unbounded`, `not_fresh` of a rollup entry): this picks the field to print and
 * formats it. "Fresh" is said only of an estimate (`fresh ~15:05`, rounded up to the minute);
 * a tick's next look is "rechecked 15:00"; a node with neither says why (`no estimate: …`),
 * never a blank.
 */

import { noEstimateWord, settledByATick } from './causeDisplay'
import { formatEstimate, formatSlot, type TimeZone } from './time'
import type { RollupEntry } from './generated/RollupEntry'
import type { NodeState } from './types'

/** What a row says about when it is fresh, and the full sentence for a tooltip. */
export interface EstimateWords {
  /** `fresh ~15:05`, `rechecked 15:00`, `no estimate: no refresh path`; empty for nothing to say. */
  text: string
  /** Whether it is a promise (`fresh`), a look (`rechecked`), a reason, or a fresh node's state. */
  kind: 'fresh' | 'rechecked' | 'none' | 'ok'
  title: string
}

type Timed = Pick<NodeState, 'expected_fresh_at' | 'recheck_at' | 'no_estimate' | 'cause'>

/** A node's estimate: the first of its expected time, its next recheck, and why it has neither. */
export function nodeEstimate(node: Timed, zone: TimeZone, nowMs: number): EstimateWords {
  if (node.expected_fresh_at !== null) {
    return { kind: 'fresh', text: `fresh ${formatEstimate(node.expected_fresh_at, zone, nowMs)}`, title: 'When it is expected to be fresh: an estimate, rounded up to the minute' }
  }
  const recheck = node.recheck_at === null ? null : `rechecked ${formatSlot(node.recheck_at, zone, nowMs)}`
  if (node.cause === null) {
    return { kind: 'ok', text: recheck ?? 'fresh', title: recheck ? 'It is fresh; the next tick of its refresh path looks at it then' : '' }
  }
  const why = node.no_estimate === null ? null : `no estimate: ${noEstimateWord(node.no_estimate)}`
  if (recheck !== null && settledByATick(node.no_estimate)) {
    return { kind: 'rechecked', text: recheck, title: `A tick looks at it then; it may be found up to date, rebuilt or left as it is${why ? ` (${why})` : ''}` }
  }
  return { kind: 'none', text: node.no_estimate === 'no_path' ? 'no estimate' : why ?? 'no estimate', title: node.no_estimate === 'no_path' ? 'No refresh path is configured' : recheck ? `Nothing known will leave it fresh; ${recheck} by the next tick of its refresh path` : 'Nothing known will leave it fresh' }
}

type Scoped = Pick<RollupEntry, 'fresh_by' | 'unbounded' | 'not_fresh'>

/**
 * A scope's estimate: `fresh` when nothing underneath is behind, `all fresh by ~15:02` when
 * every node behind has an estimate, `152 with no estimate` when none has, and both when some
 * have (`152 with no estimate · the rest fresh by ~15:02`).
 */
export function scopeEstimate(entry: Scoped, zone: TimeZone, nowMs: number): EstimateWords {
  if (entry.not_fresh === 0) return { kind: 'ok', text: 'fresh', title: 'Every node underneath is fresh' }
  const none = entry.unbounded > 0 ? `${entry.unbounded.toLocaleString()} with no estimate` : null
  const by = entry.fresh_by === null ? null : formatEstimate(entry.fresh_by, zone, nowMs)
  if (by !== null && none === null) return { kind: 'fresh', text: `all fresh by ${by}`, title: 'When every node underneath that is not fresh is expected fresh: the latest of their estimates' }
  if (by !== null) return { kind: 'none', text: `${none} · the rest fresh by ${by}`, title: 'Nothing known will leave the nodes with no estimate fresh; the others are expected fresh by the time given' }
  return { kind: 'none', text: none ?? 'no estimate', title: 'Nothing known will leave these nodes fresh: failing, undecided, missed by a tick or with no refresh path' }
}
