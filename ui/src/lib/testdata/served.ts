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
 * fixture about a health no row has on its own (`blocked` by a node upstream, `suspect`, a late
 * schedule) states it with its reason and cause: `blockedBy(id)` and
 * `LATE_SCHEDULE` read them off rows of the generated project (`project.ts`), so what a blocked
 * or late node is told is what the server tells one. There is no table from a health to "its"
 * reason or cause here: the server has none.
 */

import type { Cause } from '../generated/Cause'
import type { Health } from '../generated/Health'
import type { HealthReason } from '../generated/HealthReason'
import type { NodeState } from '../types'
import table from './health-alone.json'
import { row as served_row } from './project'

type Verdict = 'health' | 'health_reason' | 'cause'
type Classified = Verdict | 'measured' | 'root' | 'gap_seconds' | 'last_check' | 'refresh_path' | 'stale_since' | 'expected_fresh_at' | 'recheck_at' | 'no_estimate'
export type Unserved = Omit<NodeState, Classified> & Partial<Pick<NodeState, Classified>>

const ALONE = table as Record<string, { health: Health; health_reason: HealthReason; cause: Cause | null }>

/** What `health::classify` says of `row` on its own: a lookup, by the facts the fixture states. */
function alone(row: Unserved): [Health, HealthReason, Cause | null] {
  const last = row.last_materialization === null ? 'none' : row.last_materialization.status
  const key = `${row.cache.state}|${last}|${(row.partitions?.failed ?? 0) > 0}`
  const found = ALONE[key]
  if (!found) throw new Error(`no row ${key} in health-alone.json: regenerate it (crates/barca-core/src/ui_fixture_tests.rs)`)
  return [found.health, found.health_reason, found.cause]
}

/** `row` with the health, reason and cause the server gives it on its own, unless the fixture states them. */
export function served(row: Unserved): NodeState {
  // A fixture that states a health states its reason and cause with it: none is made up for it.
  if (row.health !== undefined && (row.health_reason === undefined || row.cause === undefined)) {
    throw new Error(`${row.id} states its health without its health_reason or cause: state all three (blockedBy, LATE_SCHEDULE, or a row of project.ts)`)
  }
  const [aloneHealth, aloneReason, aloneCause] = alone(row)
  const health = row.health ?? aloneHealth
  const health_reason = row.health_reason ?? aloneReason
  const cause = row.cause !== undefined ? row.cause : aloneCause
  // A failing node is its own root; a fixture about a node blocked by another states its `root`.
  const root = row.root ?? (health === 'failing' ? { id: `failure:${row.id}`, kind: 'failure' as const, node: row.id } : null)
  return { ...row, root, cause, health, health_reason, gap_seconds: row.gap_seconds ?? 0, measured: row.measured ?? (row.kind === 'asset' ? false : null), last_check: row.last_check ?? null, refresh_path: row.refresh_path ?? null, stale_since: row.stale_since ?? null, expected_fresh_at: row.expected_fresh_at ?? null, recheck_at: row.recheck_at ?? null, no_estimate: row.no_estimate ?? null }
}

const verdict = ({ health, health_reason, cause }: NodeState): Pick<NodeState, Verdict> => ({ health, health_reason, cause })

/**
 * What the server says of a node the failing node `by` blocks: the health, reason and cause of
 * the generated project's blocked node (`sales_report`), with `by` as its root.
 */
export function blockedBy(by: string): Pick<NodeState, Verdict | 'root'> {
  return { ...verdict(served_row('sales_report')), root: { id: `failure:${by}`, kind: 'failure', node: by } }
}

/** What the server says of a node whose schedule is late: the generated project's `nightly_export`. */
export const LATE_SCHEDULE: Pick<NodeState, Verdict> = verdict(served_row('nightly_export'))
if (LATE_SCHEDULE.health_reason !== 'schedule_late' || served_row('sales_report').health !== 'blocked') {
  throw new Error('the generated project no longer has a late schedule (nightly_export) or a blocked node (sales_report): point LATE_SCHEDULE and blockedBy at rows that are')
}
