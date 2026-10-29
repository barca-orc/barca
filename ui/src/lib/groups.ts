/** Organizational metadata, independent of execution. */
import type { ListNode, NodeState, StatusKind } from './types'
import { shortName, type GraphEntry } from './graph'
import { graphState, liveGraphState, UNKNOWN_GRAPH_STATE, type GraphState } from './graphState'
import { runningCount, runningGraphState } from './running'

import type { NodeGroup } from './types'
export type { NodeGroup } from './types'

export interface GroupTree {
  groups: Map<string, NodeGroup>
  roots: string[]
  parents: Map<string, string>
}

export const MODELING_PIPELINE = 'modeling.py'
export const PREVIEW_FAILURE =
  'modeling.py:validate__cv__fold_03__trained_model'
/** Build an organizational tree from metadata returned by the parser. */
export function groupTree(metadata: NodeGroup[]): GroupTree {
  const groups = new Map(metadata.map(g => [g.id, g]))
  const parents = new Map<string, string>()
  for (const g of metadata) for (const member of g.members) parents.set(member, g.id)
  return { groups, roots: metadata.filter(g => !parents.has(g.id)).map(g => g.id), parents }
}

export function groupLeaves(id: string, tree: GroupTree): string[] {
  const group = tree.groups.get(id)
  return group
    ? group.members.flatMap((member) => groupLeaves(member, tree))
    : [id]
}

export function groupOutput(id: string, tree: GroupTree): string {
  const group = tree.groups.get(id)
  return group ? groupOutput(group.output, tree) : id
}

export function groupPath(id: string | null, tree: GroupTree): NodeGroup[] {
  if (!id) return []
  const parent = tree.parents.get(id)
  const group = tree.groups.get(id)
  return [...groupPath(parent ?? null, tree), ...(group ? [group] : [])]
}

export function memberName(id: string): string {
  const name = shortName(id)
  const check = name.startsWith('validate__')
  const parts = name.replace(/^validate__/, '').split('__')
  return `${check ? 'Check · ' : ''}${parts.at(-1)?.replaceAll('_', ' ') ?? name}`
}

export function memberHealth(node: NodeState | undefined): GraphState {
  if (!node) return UNKNOWN_GRAPH_STATE
  if (node.running) return runningGraphState(node.running)
  if (
    node.kind !== 'asset' &&
    node.last_materialization?.status === 'success'
  ) {
    return {
      status: 'success',
      label: 'Passed',
      hint: 'The latest execution succeeded.',
    }
  }
  return graphState(node)
}

export function groupHealth(
  ids: string[],
  states: Map<string, NodeState>,
  live: Record<string, StatusKind> = {},
): GraphState {
  const members = ids.map((id) => live[id] ? liveGraphState(live[id]) : memberHealth(states.get(id)))
  // Work in progress comes first: the group pulses while a run is inside it.
  const running = runningCount(ids, states, live)
  if (running)
    return {
      status: 'running',
      label: `${running} running`,
      hint: `A run is working on ${running} member${running === 1 ? '' : 's'} of this group now.`,
    }
  const failed = members.filter((s) => s.status === 'failed').length
  const warning = members.filter((s) => s.status === 'warning').length
  const unknown = members.filter((s) => s.status === 'skipped').length
  if (failed)
    return {
      status: 'failed',
      label: `${failed} failed member${failed === 1 ? '' : 's'}`,
      hint: 'A failure anywhere inside this group makes it unhealthy, including failed checks on cached assets.',
    }
  if (warning)
    return {
      status: 'warning',
      label: `${warning} stale / partial`,
      hint: 'One or more member assets need attention.',
    }
  if (unknown)
    return {
      status: 'skipped',
      label: `${unknown} unchecked / unknown`,
      hint: 'Some members have not run or their state is unavailable.',
    }
  return {
    status: 'success',
    label: 'Healthy',
    hint: 'All assets are cached and all checks have passed.',
  }
}

/** Collapse only visual edges. Real inputs and the original graph stay intact. */
export function projectGroups(
  assets: ListNode[],
  tree: GroupTree,
  scope: string | null,
) {
  const byId = new Map(assets.map((a) => [a.id, a]))
  const members = scope
    ? (tree.groups.get(scope)?.members ?? [])
    : [
        ...tree.roots.filter((id) =>
          groupLeaves(id, tree).some((leaf) => byId.has(leaf)),
        ),
        ...assets.filter((a) => !tree.parents.has(a.id)).map((a) => a.id),
      ]
  const owner = new Map<string, string>()
  for (const id of members)
    for (const leaf of groupLeaves(id, tree)) owner.set(leaf, id)
  const external = new Set<string>()
  const entries: GraphEntry[] = members
    .filter((id) => tree.groups.has(id) || byId.has(id))
    .map((id) => {
      const group = tree.groups.get(id)
      const leaves = groupLeaves(id, tree).filter((leaf) => byId.has(leaf))
      const inputs = new Set<string>()
      for (const leaf of leaves)
        for (const input of byId.get(leaf)?.inputs ?? []) {
          const visibleOwner = owner.get(input)
          if (visibleOwner && visibleOwner !== id) inputs.add(visibleOwner)
          else if (!visibleOwner) external.add(input)
        }
      const assetCount = leaves.filter(
        (leaf) => byId.get(leaf)?.kind === 'asset',
      ).length
      return {
        id,
        kind: group ? 'group' : byId.get(id)!.kind,
        inputs: [...inputs],
        name: group?.name ?? (scope ? memberName(id) : shortName(id)),
        summary: `${assetCount} assets · ${leaves.length - assetCount} checks`,
        outputName: group ? memberName(groupOutput(id, tree)) : undefined,
      }
    })
  return { entries, external: [...external] }
}
