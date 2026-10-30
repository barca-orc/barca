/** Organizational metadata, independent of execution. */
import type { ListNode, NodeState, StatusKind } from './types'
import { shortName, type GraphEntry } from './graph'
import { graphState, liveGraphState, UNKNOWN_GRAPH_STATE, type GraphState } from './graphState'
import { runningCount, runningGraphState } from './running'
import { isCheck } from './status'

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

/**
 * The group a grouped view has open, from its URL: `?group=`, else the selected node's
 * group, so a link to a node opens where it lives. The failures list and the flat view keep
 * the scope they were opened in (a selection there does not move it).
 */
export function viewScope(params: URLSearchParams, tree: GroupTree): string | null {
  if (params.get('flat') === '1') return null
  const selected = params.get(params.get('view') === 'graph' ? 'focus' : 'node')
  const requested = params.get('group') ?? (params.get('show') === 'failures' ? null : tree.parents.get(selected ?? ''))
  return requested && tree.groups.has(requested) ? requested : null
}

/** Every group's leaves, computed once per tree. */
export function leavesByGroup(tree: GroupTree): Map<string, string[]> {
  const out = new Map<string, string[]>()
  const visit = (id: string): string[] => {
    const known = out.get(id)
    if (known) return known
    const group = tree.groups.get(id)
    if (!group) return [id]
    const leaves = group.members.flatMap(visit)
    out.set(id, leaves)
    return leaves
  }
  for (const id of tree.groups.keys()) visit(id)
  return out
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
  const check = /^(check|validate)__/.test(name)
  const parts = name.replace(/^(check|validate)__/, '').split('__')
  return `${check ? 'Check · ' : ''}${parts.at(-1)?.replaceAll('_', ' ') ?? name}`
}

/** The ways to call a node, shortest first: `invoices_00`, `raw__invoices_00`, …, its id. */
function nameChoices(id: string): string[] {
  const short = shortName(id)
  const check = /^(check|validate)__/.exec(short)?.[1]
  const parts = (check ? short.slice(check.length + 2) : short).split('__')
  const choices = parts.map((_, i) => [...(check ? [check] : []), ...parts.slice(parts.length - 1 - i)].join('__'))
  return [...choices, id]
}

/**
 * A label per node: the part of its id that tells it from the others in the same table, so no
 * two rows read the same. `billing__raw__invoices_00` is `invoices_00` until another node ends
 * the same way.
 */
export function distinctNames(ids: readonly string[]): Map<string, string> {
  const choices = new Map(ids.map((id) => [id, nameChoices(id)]))
  const level = new Map(ids.map((id) => [id, 0]))
  const label = (id: string) => {
    const options = choices.get(id)!
    return options[Math.min(level.get(id)!, options.length - 1)]!
  }
  for (;;) {
    const holders = new Map<string, string[]>()
    for (const id of choices.keys()) {
      const same = holders.get(label(id))
      if (same) same.push(id)
      else holders.set(label(id), [id])
    }
    const clashes = [...holders.values()].filter((same) => same.length > 1)
    const movable = clashes.flat().filter((id) => level.get(id)! < choices.get(id)!.length - 1)
    if (movable.length === 0) break
    for (const id of movable) level.set(id, level.get(id)! + 1)
  }
  return new Map([...choices.keys()].map((id) => [id, label(id)]))
}

/** How a grouped table can be ordered; `worst` is the default and stays out of the URL. */
export type EntrySort = 'worst' | 'name' | 'oldest'
export const ENTRY_SORTS: readonly EntrySort[] = ['worst', 'name', 'oldest']

export function parseEntrySort(raw: string | null): EntrySort {
  return ENTRY_SORTS.find((s) => s === raw) ?? 'worst'
}

/** What a row of a grouped table is ordered by. */
export interface EntryRank {
  name: string
  failed: number
  stale: number
  lastSuccessAt: string | null
}

const RANK_NAMES = new Intl.Collator(undefined, { numeric: true })

/** Worst first (failed, then stale, then name), by name, or longest since a success (never first). */
export function compareEntries(sort: EntrySort, a: EntryRank, b: EntryRank): number {
  const name = RANK_NAMES.compare(a.name, b.name)
  if (sort === 'name') return name
  if (sort === 'oldest') {
    const at = (a.lastSuccessAt ?? '').localeCompare(b.lastSuccessAt ?? '')
    return at || name
  }
  return b.failed - a.failed || b.stale - a.stale || name
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
      label: 'Healthy',
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
      label: `${failed} failed`,
      hint: 'A failure anywhere inside this group makes it unhealthy, including failed checks on cached assets.',
    }
  if (warning)
    return {
      status: 'warning',
      label: `${warning} stale`,
      hint: 'One or more member assets need attention.',
    }
  if (unknown)
    return {
      status: 'skipped',
      label: `${unknown} not run`,
      hint: 'Some members have not run or their state is unavailable.',
    }
  return {
    status: 'success',
    label: 'Healthy',
    hint: 'Every asset is up to date and every check succeeded.',
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
  const external = new Map<string, string[]>()
  const names = distinctNames(members.filter((id) => !tree.groups.has(id)))
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
          else if (!visibleOwner) external.set(input, [...(external.get(input) ?? []), id])
        }
      const assetCount = leaves.filter(
        (leaf) => byId.get(leaf)?.kind === 'asset',
      ).length
      const checkCount = leaves.filter((leaf) => {
        const node = byId.get(leaf)
        return node && isCheck(node)
      }).length
      const others = leaves.length - assetCount - checkCount
      return {
        id,
        kind: group ? 'group' : byId.get(id)!.kind,
        inputs: [...inputs],
        name: group?.name ?? names.get(id) ?? shortName(id),
        summary: `${assetCount} assets · ${checkCount} checks${others ? ` · ${others} other` : ''}`,
        outputName: group ? memberName(groupOutput(id, tree)) : undefined,
      }
    })
  return { entries, external: [...external.keys()], consumers: external }
}

/** `1 group`, `3 groups`. */
export function plural(count: number, noun: string): string {
  return `${count.toLocaleString()} ${noun}${count === 1 ? '' : 's'}`
}

/** The group that is all of `assets`, if there is one: nothing above it is worth a level. */
export function soleGroup(assets: ListNode[], tree: GroupTree): string | null {
  if (assets.length === 0 || assets.some((a) => !tree.parents.has(a.id))) return null
  const ids = new Set(assets.map((a) => a.id))
  const roots = tree.roots.filter((id) => groupLeaves(id, tree).some((leaf) => ids.has(leaf)))
  return roots.length === 1 ? roots[0]! : null
}

/** Past this many, a view's outside inputs are shown by where they come from. */
const PORTS_UP_TO = 10

/**
 * `entries` and, as ports feeding them, the inputs from outside the view: one each, or one for
 * each place they come from (`placeOf`) when there are too many to read.
 */
export function withPorts(
  entries: GraphEntry[],
  external: ReadonlyMap<string, string[]>,
  placeOf: (id: string) => string,
): GraphEntry[] {
  const merged = external.size > PORTS_UP_TO
  const ports = new Map<string, { ids: string[]; consumers: Set<string> }>()
  for (const [input, consumers] of external) {
    const key = merged ? `port:${placeOf(input)}` : input
    const port = ports.get(key) ?? { ids: [], consumers: new Set() }
    port.ids.push(input)
    for (const id of consumers) port.consumers.add(id)
    ports.set(key, port)
  }
  const fed = new Map<string, string[]>()
  for (const [key, { consumers }] of ports)
    for (const id of consumers) fed.set(id, [...(fed.get(id) ?? []), key])
  return [
    ...[...ports].map(([key, { ids, consumers }]): GraphEntry => ({
      id: key,
      kind: 'port',
      inputs: [],
      name: merged ? `${key.slice('port:'.length)} · ${ids.length}` : shortName(key),
      summary: merged ? ids.map(shortName).join(', ') : undefined,
      loose: consumers.size > 1,
    })),
    ...entries.map((e) => (fed.has(e.id) ? { ...e, inputs: [...e.inputs, ...fed.get(e.id)!] } : e)),
  ]
}
