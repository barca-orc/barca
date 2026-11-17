/**
 * Mocked rows of `GET /state` as the server would serve them. Health is classified in
 * `barca-core` and arrives on every row (`health`, `health_reason`, `signals`, `measured`,
 * `root`, `last_check`); the UI derives none of it, so a mock that leaves it out is not a row the server
 * sends. `served` fills in what the server says of a row with these facts and nothing around
 * it (a lookup in `src/lib/testdata/health-alone.json`, which a Rust test generates); a mock about a node a failure blocks states that itself (`blockedBy`).
 */

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

interface Row {
  id: string
  kind?: string
  cache: { state: string }
  last_materialization?: { status: string } | null
  partitions?: { failed?: number } | null
  health?: string
}

// What `health::classify` says of a row alone, for every combination of these facts: written
// by a Rust test (`crates/barca-core/src/ui_fixture_tests.rs`), so no health rule is spelled here.
const ALONE = JSON.parse(
  readFileSync(fileURLToPath(new URL('../../src/lib/testdata/health-alone.json', import.meta.url)), 'utf8'),
) as Record<string, { health: string; health_reason: string }>

function alone(row: Row): [string, string] {
  const key = `${row.cache.state}|${row.last_materialization?.status ?? 'none'}|${(row.partitions?.failed ?? 0) > 0}`
  const found = ALONE[key]
  if (!found) throw new Error(`no row ${key} in health-alone.json: regenerate it (crates/barca-core/src/ui_fixture_tests.rs)`)
  return [found.health, found.health_reason]
}

/** `row` with the health the server gives it on its own, unless the mock states one. */
export function served<T extends Row>(row: T) {
  const [health, health_reason] = alone(row)
  const root = health === 'failing' ? { id: `failure:${row.id}`, kind: 'failure', node: row.id } : null
  return { health, health_reason, signals: [], measured: row.kind === 'asset' ? false : null, root, last_check: null, ...row }
}

/** What the server says of a node the failing node `by` blocks. */
export function blockedBy(by: string) {
  return { health: 'blocked', health_reason: 'upstream_failing', root: { id: `failure:${by}`, kind: 'failure', node: by } }
}
