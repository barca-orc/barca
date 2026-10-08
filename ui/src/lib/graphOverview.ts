import { match } from 'ts-pattern'
import { severityOf } from './assetTable'
import { statusMeta } from './status'
import type { AssetSummary, NodeState, StatusKind } from './types'

export interface GraphState { status: StatusKind; label: string; hint: string }
export const UNKNOWN_GRAPH_STATE: GraphState = { status: 'skipped', label: 'State unavailable', hint: 'Asset state has not been loaded.' }

/** Resting colors describe cache state; a failed latest attempt takes priority. */
export function graphState(node: NodeState): GraphState {
  const state = match(severityOf(node))
    .with('failed', () => ({ status: 'failed' as const, label: 'Last attempt failed' }))
    .with('cached', () => ({ status: 'success' as const, label: 'Cached' }))
    .with('stale', () => ({ status: 'warning' as const, label: 'Stale' }))
    .with('partial', () => ({ status: 'warning' as const, label: 'Partial cache' }))
    .with('never_run', () => ({ status: 'skipped' as const, label: 'Never run' }))
    .with('always_runs', () => ({ status: 'skipped' as const, label: 'Always runs' }))
    .with('unknown', () => ({ status: 'skipped' as const, label: 'State unknown' }))
    .exhaustive()
  return { ...state, hint: state.status === 'failed' ? node.last_materialization?.error ?? state.label : node.cache.detail }
}

/** Live overlays are separate from resting cache state and keep their own labels. */
export function liveGraphState(status: StatusKind): GraphState {
  return { status, label: statusMeta(status).label, hint: 'Current run' }
}

export interface Overview { assets: AssetSummary[]; groups: Record<string, string[]>; allGroups: Record<string, string[]> }

/** Collapse only maximal linear chains of unscheduled, unpartitioned assets.
 * Roots, leaves, branches, joins, tasks, sensors and scope boundaries remain visible.
 * Retain cycles unchanged (invalid DAGs should never disappear into a group).
 */
export function graphOverview(
  assets: AssetSummary[], partitioned: ReadonlySet<string> = new Set(),
  expanded: ReadonlySet<string> = new Set(), selected: string | null = null,
): Overview {
  const byId = new Map(assets.map(a => [a.id, a]))
  const outputs = new Map(assets.map(a => [a.id, [] as string[]]))
  for (const a of assets) for (const input of new Set(a.inputs)) outputs.get(input)?.push(a.id)
  const eligible = new Set(assets.filter(a => a.kind === 'asset' && a.freshness.type !== 'Schedule'
    && !partitioned.has(a.id) && a.inputs.length === 1 && byId.has(a.inputs[0]!)
    && outputs.get(a.id)?.length === 1).map(a => a.id))
  const allGroups: Record<string, string[]> = {}
  for (const a of assets) {
    if (!eligible.has(a.id) || eligible.has(a.inputs[0]!)) continue
    const members: string[] = []
    let id: string | undefined = a.id
    const seen = new Set<string>()
    while (id && eligible.has(id) && !seen.has(id)) {
      seen.add(id); members.push(id); id = outputs.get(id)?.[0]
    }
    if (members.length >= 2) allGroups[`chain:${JSON.stringify(members)}`] = members
  }
  const groups = Object.fromEntries(Object.entries(allGroups).filter(([id, members]) => !expanded.has(id) && !members.includes(selected ?? '')))
  const membership = new Map(Object.entries(groups).flatMap(([id, members]) => members.map(m => [m, id] as const)))
  const visible = assets.flatMap(a => {
    const group = membership.get(a.id)
    if (group && groups[group]![0] !== a.id) return []
    return [{ ...a, id: group ?? a.id, inputs: [...new Set(a.inputs.map(input => membership.get(input) ?? input))] }]
  })
  return { assets: visible, groups, allGroups }
}

/** Never paint a group green while a member needs attention. */
export function groupState(members: string[], states: Record<string, GraphState>): GraphState {
  const entries = members.map(id => states[id] ?? UNKNOWN_GRAPH_STATE)
  const rank: StatusKind[] = ['failed', 'running', 'warning', 'queued', 'pending', 'skipped', 'success']
  const status = rank.find(s => entries.some(e => e.status === s)) ?? 'skipped'
  const counts = new Map<string, number>()
  for (const e of [...entries].sort((a, b) => rank.indexOf(a.status) - rank.indexOf(b.status))) counts.set(e.label, (counts.get(e.label) ?? 0) + 1)
  const label = [...counts].map(([name, count]) => `${count} ${name.toLowerCase()}`).join(' · ')
  return { status, label, hint: entries.map((e, i) => `${members[i]}: ${e.label}`).join('\n') }
}
