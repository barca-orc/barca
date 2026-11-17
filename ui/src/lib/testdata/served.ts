/**
 * Fixture rows as the server would serve them. Test and kit data only: nothing the app ships
 * imports this (the layering guard, `crates/barca-cli/src/ui_layering_tests.rs`, checks it).
 *
 * Health is classified in `barca-core` (`crates/barca-core/src/health.rs`) and arrives on every
 * row. A fixture that spells a row by hand would have to repeat `health` beside the facts that
 * decide it; `served` fills it in. No health rule is written here: the word of a row with these
 * facts and no evidence around it (no graph, no signals, no schedule, no tick) is looked up in
 * `health-alone.json`, which a Rust test generates by running `health::classify` on each such
 * row (`crates/barca-core/src/ui_fixture_tests.rs`) and fails when the file is out of date. A
 * fixture about `blocked` by a node upstream, `suspect` or a late schedule states its `health`
 * itself (and, for a node a failure blocks, its `root`: `blockedBy(id)` spells one).
 */

import type { Health } from '../generated/Health'
import type { HealthReason } from '../generated/HealthReason'
import type { NodeState } from '../types'
import table from './health-alone.json'

type Classified = 'health' | 'health_reason' | 'measured' | 'root' | 'last_check' | 'refresh_path' | 'stale_since' | 'expected_fresh_at' | 'recheck_at'
export type Unserved = Omit<NodeState, Classified> & Partial<Pick<NodeState, Classified>>

const ALONE = table as Record<string, { health: Health; health_reason: HealthReason }>

/** What `health::classify` says of `row` on its own: a lookup, by the facts the fixture states. */
function alone(row: Unserved): [Health, HealthReason] {
  const last = row.last_materialization === null ? 'none' : row.last_materialization.status
  const key = `${row.cache.state}|${last}|${(row.partitions?.failed ?? 0) > 0}`
  const found = ALONE[key]
  if (!found) throw new Error(`no row ${key} in health-alone.json: regenerate it (crates/barca-core/src/ui_fixture_tests.rs)`)
  return [found.health, found.health_reason]
}

/** `row` with the health the server gives it on its own, unless the fixture states one. */
export function served(row: Unserved): NodeState {
  const [health, reason] = alone(row)
  // A failing node is its own root; a fixture about a node blocked by another states its `root`.
  const root = row.root ?? ((row.health ?? health) === 'failing' ? { id: `failure:${row.id}`, kind: 'failure' as const, node: row.id } : null)
  return { ...row, root, health: row.health ?? health, health_reason: row.health_reason ?? (row.health ? REASON[row.health] : reason), measured: row.measured ?? (row.kind === 'asset' ? false : null), last_check: row.last_check ?? null, refresh_path: row.refresh_path ?? null, stale_since: row.stale_since ?? null, expected_fresh_at: row.expected_fresh_at ?? null, recheck_at: row.recheck_at ?? null }
}

/** The reason a fixture that states a health without one gets: a default of the fixtures, not a rule. */
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
