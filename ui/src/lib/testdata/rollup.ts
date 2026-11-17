/**
 * Rollup fixtures written out as `GET /state/rollup` answers them. Nothing here sums: a test
 * or a kit page states each entry's counts, as the server would.
 */
import type { HealthCounts, HeldBy, Rollup, RollupEntry, RollupRoot } from '../rollupView'
import type { Cause, CauseCounts } from '../causeDisplay'
import type { Health } from '../healthDisplay'
import type { FileRollup } from '../generated/FileRollup'
import type { Weights } from '../generated/Weights'
import WEIGHTS from './weights.json'

/**
 * The weights an entry of `nodes` nodes with `causes` not fresh is served with ("Distance from
 * fresh"): the table is `barca-core`'s, written out by a Rust test (`weights.json`), so a
 * fixture states what the server would and the app holds no weight of its own.
 */
export function weightsOf(nodes: number, causes: CauseCounts): Weights {
  const behind = causes.failing + causes.overdue + causes.no_refresh_path + causes.queued + causes.undecided + causes.catching_up
  return {
    failing: causes.failing * WEIGHTS.failing, overdue: causes.overdue * WEIGHTS.overdue, no_refresh_path: causes.no_refresh_path * WEIGHTS.no_refresh_path,
    queued: causes.queued * WEIGHTS.queued, undecided: causes.undecided * WEIGHTS.undecided, catching_up: causes.catching_up * WEIGHTS.catching_up,
    fresh: (nodes - behind) * WEIGHTS.fresh,
  }
}

/** A `Weights` added up: an entry's `weight`. */
export function weightOf(weights: Weights): number {
  return weights.failing + weights.overdue + weights.no_refresh_path + weights.queued + weights.undecided + weights.catching_up + weights.fresh
}

/** `entry` with the `weight` and `weights` its own `nodes` and `causes` are served with, unless it states them. */
function weighed(entry: Omit<RollupEntry, 'weight' | 'weights'> & Partial<Pick<RollupEntry, 'weight' | 'weights'>>): RollupEntry {
  const weights = entry.weights ?? weightsOf(entry.nodes, entry.causes)
  return { ...entry, weights, weight: entry.weight ?? weightOf(weights) }
}

/** A file's sums, as `files` of the rollup states them: every node in `worst` unless `health` says otherwise. */
export function fileSums(file: string, worst: Health, over: Partial<FileRollup> = {}): FileRollup {
  const nodes = over.nodes ?? 1
  const health = over.health ?? counts({ [worst]: nodes })
  return { file, nodes, assets: nodes, running: 0, health, asset_health: health, worst, kinds: NO_KINDS, ...over }
}

/** No node with a cause: every node fresh, as a fixture that states none says. */
export const NO_CAUSES: RollupEntry['causes'] = { catching_up: 0, queued: 0, no_refresh_path: 0, overdue: 0, failing: 0, undecided: 0 }

/** No node of any kind: what a fixture that is not about the kinds states. */
export const NO_KINDS: Rollup['kinds'] = {
  asset: { nodes: 0, fresh: 0, causes: NO_CAUSES }, task: { nodes: 0, fresh: 0, causes: NO_CAUSES }, sensor: { nodes: 0, fresh: 0, causes: NO_CAUSES },
}

/** Cause counts with the parts given, as an entry states them. */
export function causeCounts(part: Partial<CauseCounts> = {}): CauseCounts {
  return { ...NO_CAUSES, ...part }
}

/** What a root holds under an entry, as `held_by` states it: `cumulative_nodes` is stated, not added up. */
export function heldBy(root: string, nodes: number, cumulative: number, over: Partial<HeldBy> = {}): HeldBy {
  const causes = over.causes ?? NO_CAUSES
  return { root, nodes, assets: nodes, causes, gap_seconds: 0, unbounded: 0, weight: weightOf({ ...weightsOf(0, causes), fresh: 0 }), cumulative_nodes: cumulative, ...over }
}

export function counts(part: Partial<HealthCounts> = {}): HealthCounts {
  return { failing: 0, blocked: 0, suspect: 0, behind: 0, undecided: 0, never_run: 0, ok: 0, ...part }
}

/** A node entry: one node in `health`, an asset unless `task`; not fresh, with `cause`, when one is given. */
export function nodeEntry(id: string, parent: string, health: Health, over: Partial<RollupEntry> & { task?: boolean; cause?: Cause } = {}): RollupEntry {
  const { task = false, cause, ...rest } = over
  const one = counts({ [health]: 1 })
  const why = cause ? causeCounts({ [cause]: 1 }) : NO_CAUSES
  return weighed({
    id, kind: 'node', label: id.slice(id.lastIndexOf(':') + 1), parent, children: [], nodes: 1, assets: task ? 0 : 1, running: 0,
    health: one, asset_health: task ? counts() : one, data_coverage: { assets: task ? 0 : 1, measured: 0, suspect: 0 },
    causes: why, asset_causes: task ? NO_CAUSES : why, not_fresh: cause ? 1 : 0, held_by: [], gap_seconds: 0, unbounded: 0, fresh_by: null, also_in: [], ...rest,
  })
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
  return weighed({
    id, kind, label: kind === 'root' ? 'all' : id.slice(id.indexOf(':') + 1), parent, children, nodes, assets: nodes, running: 0,
    health: h, asset_health: h, data_coverage: { assets: nodes, measured: 0, suspect: 0 },
    causes: NO_CAUSES, asset_causes: NO_CAUSES, not_fresh: 0, held_by: [], gap_seconds: 0, unbounded: 0, fresh_by: null, also_in: [], ...over,
  })
}

export function rollup(entries: RollupEntry[], roots: RollupRoot[] = [], files: FileRollup[] = []): Rollup {
  return { as_of: '2026-11-17T14:57:00Z', entries, roots, data_coverage: entries[0]!.data_coverage, files, kinds: NO_KINDS }
}

export function failureRoot(node: string, detail: string, nodes = 1, assets = nodes): RollupRoot {
  return {
    id: `failure:${node}`, kind: 'failure', node, detail, error_group: detail.replace(/\d[\d,._]*\d|\d/g, '#'), nodes, assets,
    causes: { ...NO_CAUSES, failing: nodes }, gap_seconds: 0, unbounded: nodes, weight: nodes * WEIGHTS.failing,
    action: { kind: 'fix_failure', command: `barca get ${node.slice(node.lastIndexOf(':') + 1)} --server`, estimated_seconds: null },
  }
}

/** A root that is not a failure, as the server words it. */
export function heldRoot(over: Pick<RollupRoot, 'kind' | 'node' | 'detail' | 'nodes' | 'action'> & Partial<RollupRoot>): RollupRoot {
  const causes = over.causes ?? NO_CAUSES
  return { id: `${over.kind}:${over.node}`, error_group: null, assets: over.nodes, causes, gap_seconds: 0, unbounded: 0, weight: weightOf({ ...weightsOf(0, causes), fresh: 0 }), ...over }
}
