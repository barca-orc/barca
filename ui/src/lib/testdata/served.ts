/**
 * Fixture rows as the server would serve them. Test and kit data only: nothing the app ships
 * imports this.
 *
 * Health is classified in `barca-core` (`crates/barca-core/src/health.rs`) and arrives on every
 * row. A fixture that spells a row by hand would have to repeat `health` beside the facts that
 * decide it, and could get it wrong; `served` fills in the word the server gives a row with
 * these facts and no evidence around it (no graph, no signals, no schedule, no tick). A fixture about
 * `blocked` by a node upstream, `suspect` or a late schedule states its `health` itself.
 */

import type { Health } from '../generated/Health'
import type { HealthReason } from '../generated/HealthReason'
import type { NodeState } from '../types'

type Classified = 'health' | 'health_reason' | 'measured' | 'last_check'
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
  return { ...row, health: row.health ?? health, health_reason: row.health_reason ?? (row.health ? REASON[row.health] : reason), measured: row.measured ?? (row.kind === 'asset' ? false : null), last_check: row.last_check ?? null }
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
