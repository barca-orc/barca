/**
 * Fixture rows as the server would serve them. Test and kit data only: nothing the app ships
 * imports this.
 *
 * Health is classified in `barca-core` (`crates/barca-core/src/health.rs`) and arrives on every
 * row. A fixture that spells a row by hand would have to repeat `health` beside the facts that
 * decide it, and could get it wrong; `served` fills in the word the server gives a row with
 * these facts and no evidence around it (no graph, no signals, no schedule). A fixture about
 * `blocked` by a node upstream, `suspect` or a late schedule states its `health` itself (and,
 * for a node a failure blocks, its `root`: `blockedBy(id)` spells one).
 */

import type { Health } from '../generated/Health'
import type { HealthReason } from '../generated/HealthReason'
import type { NodeState } from '../types'

type Classified = 'health' | 'health_reason' | 'measured' | 'root'
export type Unserved = Omit<NodeState, Classified> & Partial<Pick<NodeState, Classified>>

function alone(row: Unserved): [Health, HealthReason] {
  if (row.last_materialization?.status === 'failed') return ['failing', 'last_attempt_failed']
  if ((row.partitions?.failed ?? 0) > 0) return ['failing', 'keys_failed']
  switch (row.cache.state) {
    case 'blocked': return ['blocked', 'cache_blocked']
    case 'stale': return ['behind', 'stale']
    case 'partial': return ['behind', 'partial']
    case 'unsynced': return ['behind', 'unsynced']
    case 'unknown': return ['undecided', 'unknown']
    case 'never_run': return ['never_run', 'never_run']
    case 'always_runs': return row.last_materialization ? ['ok', 'last_run_succeeded'] : ['never_run', 'no_successful_run']
    case 'cached': return ['ok', 'cached']
  }
}

/** `row` with the health the server gives it on its own, unless the fixture states one. */
export function served(row: Unserved): NodeState {
  const [health, reason] = alone(row)
  // A failing node is its own root; a fixture about a node blocked by another states its `root`.
  const root = row.root ?? ((row.health ?? health) === 'failing' ? { id: `failure:${row.id}`, kind: 'failure' as const, node: row.id } : null)
  return { ...row, root, health: row.health ?? health, health_reason: row.health_reason ?? (row.health ? REASON[row.health] : reason), measured: row.measured ?? (row.kind === 'asset' ? false : null) }
}

/** The usual reason of a health a fixture states without one. */
const REASON = {
  failing: 'last_attempt_failed',
  blocked: 'upstream_failing',
  suspect: 'rows_changed',
  behind: 'stale',
  undecided: 'unknown',
  never_run: 'never_run',
  ok: 'cached',
} satisfies Record<Health, HealthReason>

/** What the server says of a node the failing node `by` blocks: `blocked`, with `by` as its root. */
export function blockedBy(by: string): Pick<NodeState, 'health' | 'health_reason' | 'root'> {
  return { health: 'blocked', health_reason: 'upstream_failing', root: { id: `failure:${by}`, kind: 'failure', node: by } }
}
