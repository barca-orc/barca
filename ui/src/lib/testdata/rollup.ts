/**
 * Rollup fixtures written out as `GET /state/rollup` answers them. Nothing here sums: a test
 * or a kit page states each entry's counts, as the server would.
 */
import type { HealthCounts, Rollup, RollupEntry, RollupRoot } from '../rollupView'
import type { Health } from '../healthDisplay'

export function counts(part: Partial<HealthCounts> = {}): HealthCounts {
  return { failing: 0, blocked: 0, suspect: 0, behind: 0, undecided: 0, never_run: 0, ok: 0, ...part }
}

/** A node entry: one node in `health`, an asset unless `task`. */
export function nodeEntry(id: string, parent: string, health: Health, over: Partial<RollupEntry> & { task?: boolean } = {}): RollupEntry {
  const { task = false, ...rest } = over
  const one = counts({ [health]: 1 })
  return {
    id, kind: 'node', label: id.slice(id.lastIndexOf(':') + 1), parent, children: [], nodes: 1, assets: task ? 0 : 1, running: 0,
    health: one, asset_health: task ? counts() : one, data_coverage: { assets: task ? 0 : 1, measured: 0, suspect: 0 }, ...rest,
  }
}

/** A root, group or file entry with the counts given: `asset_health` is `health` unless stated. */
export function scopeEntry(
  id: string,
  kind: RollupEntry['kind'],
  parent: string | null,
  children: string[],
  health: Partial<HealthCounts>,
  over: Partial<RollupEntry> = {},
): RollupEntry {
  const h = counts(health)
  const nodes = h.failing + h.blocked + h.suspect + h.behind + h.undecided + h.never_run + h.ok
  return {
    id, kind, label: kind === 'root' ? 'all' : id.slice(id.indexOf(':') + 1), parent, children, nodes, assets: nodes, running: 0,
    health: h, asset_health: h, data_coverage: { assets: nodes, measured: 0, suspect: 0 }, ...over,
  }
}

export function rollup(entries: RollupEntry[], roots: RollupRoot[] = []): Rollup {
  return { as_of: '2026-11-17T14:57:00Z', entries, roots, data_coverage: entries[0]!.data_coverage }
}

export function failureRoot(node: string, detail: string, nodes = 1, assets = nodes): RollupRoot {
  return { id: `failure:${node}`, kind: 'failure', node, detail, error_group: detail.replace(/\d[\d,._]*\d|\d/g, '#'), nodes, assets }
}
