/**
 * Mocked rows of `GET /state` as the server would serve them. Health is classified in
 * `barca-core` and arrives on every row (`health`, `health_reason`, `signals`, `measured`,
 * `root`, `last_check`); the UI derives none of it, so a mock that leaves it out is not a row the server
 * sends. `served` fills in what the server says of a row with these facts and nothing around
 * it; a mock about a node a failure blocks states that itself (`blockedBy`).
 */

interface Row {
  id: string
  kind?: string
  cache: { state: string }
  last_materialization?: { status: string } | null
  partitions?: { failed?: number } | null
  health?: string
}

function alone(row: Row): [string, string] {
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
    default: return ['ok', 'cached']
  }
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
