/**
 * The grouped pipeline page's derivations, pure: what the URL asks for, which scope that opens,
 * the entries of that level and what a search adds to them, each entry's box, the failures in
 * scope, and the URL changes each action makes. `pages/GroupedPipelinePage.tsx` fetches, runs the
 * effects and renders; everything it decides is here, so it can be tested without a browser.
 */

import { shortName, type GraphEntry } from './graph'
import { liveGraphState, memberHealth, PENDING_GRAPH_STATE, type GraphState } from './graphState'
import {
  compareEntries,
  groupHealth,
  groupPath,
  leavesByGroup,
  parseEntrySort,
  projectGroups,
  soleGroup,
  viewScope,
  withPorts,
  type EntryRank,
  type EntrySort,
  type GroupTree,
} from './groups'
import { consumersOf, isFailed, needsAttention, scopeAge, type Failure } from './health'
import type { RollupTree } from './rollupView'
import { GRAPH_LIMIT, lineage } from './lineage'
import { inPipeline, pipelineName, sourceFile } from './pipeline'
import { plural } from './plural'
import type { ListNode, NodeState, StatusKind } from './types'

/** How many nodes a search lists beyond the open level. */
const SEARCH_LIMIT = 500
/** `x_2` before `x_10`; one collator, not one per comparison. */
const NAME_ORDER = new Intl.Collator(undefined, { numeric: true })

export const SORT_LABEL: Record<EntrySort, string> = { worst: 'worst first', name: 'name', oldest: 'oldest success' }

type GroupedShow = 'items' | 'failures'
type GroupedMode = 'groups' | 'flat' | 'failures'

/** What the URL asks the page for. */
export interface GroupedParams {
  pipeline: string | null
  graph: boolean
  flat: boolean
  show: GroupedShow
  /** `?node=` in the list, `?focus=` in the graph. */
  selected: string | null
  /** `?group=`, or the selected node's group, when the tree has it. */
  requestedScope: string | null
  query: string
  sort: EntrySort
  dir: 'LR' | 'TB'
}

export function parseGroupedParams(params: URLSearchParams, tree: GroupTree): GroupedParams {
  const graph = params.get('view') === 'graph'
  const flat = params.get('flat') === '1'
  return {
    pipeline: params.get('pipeline'),
    graph,
    flat,
    show: !flat && params.get('show') === 'failures' ? 'failures' : 'items',
    selected: params.get(graph ? 'focus' : 'node'),
    requestedScope: viewScope(params, tree),
    query: params.get('q') ?? '',
    sort: parseEntrySort(params.get('sort')),
    dir: params.get('dir') === 'TB' ? 'TB' : 'LR',
  }
}

/** A change to the URL: a key to set, or `null` to drop. */
export type ParamChanges = Record<string, string | null>

/** `params` with `changes` applied. */
export function applyChanges(params: URLSearchParams, changes: ParamChanges): URLSearchParams {
  const next = new URLSearchParams(params)
  for (const [key, value] of Object.entries(changes)) {
    if (value === null) next.delete(key)
    else next.set(key, value)
  }
  return next
}

/** Select `id` (null clears) in the open view. */
export function selectChanges(graph: boolean, id: string | null): ParamChanges {
  return { [graph ? 'focus' : 'node']: id }
}

/** Open group `id` (null: the root). */
export function openGroupChanges(id: string | null, sole: string | null): ParamChanges {
  return { group: id === sole ? null : id, node: null, focus: null, q: null, show: null }
}

/** Jump to node `id` where it lives: its group open, the node selected. */
export function revealChanges(id: string, tree: GroupTree, graph: boolean): ParamChanges {
  return { group: tree.parents.get(id) ?? null, node: graph ? null : id, focus: graph ? id : null, q: null, show: null }
}

/** Switch between groups, every node flat, and the failures list (which keeps the open scope). */
export function modeChanges(mode: GroupedMode, scope: string | null): ParamChanges {
  if (mode === 'flat') return { flat: '1', group: null, node: null, focus: null, q: null, show: null }
  if (mode === 'failures') return { flat: null, show: 'failures', view: null, group: scope, node: null, focus: null }
  return { flat: null, show: null, node: null, focus: null }
}

/** Flip the graph's layout direction. */
export function dirChanges(dir: 'LR' | 'TB'): ParamChanges {
  return { dir: dir === 'LR' ? 'TB' : null }
}

/**
 * The row an arrow key moves the selection to: the next (or previous) selectable row, skipping
 * one that cannot be selected (a cause heading, `null`); none at the ends.
 */
export function nextRowIndex(rowIds: readonly (string | null)[], selected: string, key: 'ArrowUp' | 'ArrowDown'): number | null {
  const index = rowIds.indexOf(selected)
  if (index < 0) return null
  const step = key === 'ArrowDown' ? 1 : -1
  let next = index + step
  if (rowIds[next] === null) next += step
  return rowIds[next] ? next : null
}

/** What the page has loaded, as the derivation reads it. */
interface GroupedInput {
  params: GroupedParams
  tree: GroupTree
  /** Every node served (`GET /assets`); empty until loaded. */
  allAssets: readonly ListNode[]
  /** Every state loaded for the scope (`GET /state`). */
  states: readonly NodeState[]
  /** The state request failed: a missing state is not one still loading. */
  statesFailed: boolean
  /** The run this page streams: a status per node it has reached. */
  live: Readonly<Record<string, StatusKind>>
  /** The rollup (`useRollup`): a group's box and rank are its entry, so they agree with the health map. */
  rollup?: RollupTree
}

/** The level the page shows: its scope and what is drawn or listed in it. */
interface GroupedLevel {
  /** The nodes of the open pipeline. */
  assets: ListNode[]
  byId: Map<string, NodeState>
  leavesOf: Map<string, string[]>
  /** The group all of the pipeline is, when it is one: opened at once, left out of the breadcrumb. */
  sole: string | null
  scope: string | null
  /** The entries of the open level: groups and nodes, or every node when flat. */
  entries: GraphEntry[]
  /** Inputs of the open group from outside it. */
  external: string[]
  /** A graph too big to draw: the lineage of the selected node is drawn, or a picker shown. */
  tooBig: boolean
  lineageOf: string | null
  /** What the graph draws. */
  drawn: GraphEntry[]
  /** Nodes deeper than this level that match the search, as rows with their path. */
  hits: GraphEntry[]
  /** Each node's direct consumers, across every pipeline. */
  consumers: Map<string, string[]>
  /** The box of every entry listed or drawn. */
  visualStates: Record<string, GraphState>
  /** The nodes of the scope, and their states. */
  scopeIds: string[]
  scopedStates: NodeState[]
  /** Entries in the chosen order, those matching the search, then the hits. */
  visibleEntries: GraphEntry[]
  isHit: Set<string>
  /** Where a node is: its group path, else its file. */
  pathOf: (id: string) => string
  placeOf: (id: string) => string
  leaves: (id: string) => string[]
}

/** The leaves of `id`: a group's, or the node itself. */
const leavesIn = (leavesOf: Map<string, string[]>) => (id: string) => leavesOf.get(id) ?? [id]

export function groupedLevel({ params, tree, allAssets, states, statesFailed, live, rollup }: GroupedInput): GroupedLevel {
  const { pipeline, graph, flat, show, selected, requestedScope, query } = params
  const leavesOf = leavesByGroup(tree)
  const leaves = leavesIn(leavesOf)
  const assets = allAssets.filter((a) => inPipeline(a.id, pipeline))
  const byKind = new Map(assets.map((a) => [a.id, a.kind]))
  const byId = new Map(states.map((s) => [s.id, s]))
  const sole = soleGroup(assets, tree)
  const scope = requestedScope ?? (flat ? null : sole)
  const projected = projectGroups(assets, tree, scope)
  const entries: GraphEntry[] = flat ? assets.map((a) => ({ ...a, name: shortName(a.id) })) : projected.entries
  // The graph draws what the list lists at every level, All pipelines included: its groups and
  // loose nodes, with the edges between them.
  const tooBig = graph && entries.length > GRAPH_LIMIT
  const lineageOf = tooBig && selected && !tree.groups.has(selected) ? selected : null
  // Lineage crosses pipelines: a consumer in another file is still downstream.
  const consumers = consumersOf(allAssets)
  const pathOf = (id: string) => groupPath(tree.parents.get(id) ?? null, tree).map((g) => g.name).join(' › ')
  const placeOf = (id: string) => groupPath(tree.parents.get(id) ?? null, tree).filter((g) => g.id !== sole).map((g) => g.name).join(' › ') || sourceFile(id)
  const drawn: GraphEntry[] = tooBig
    ? lineageOf ? lineage(lineageOf, assets, 2, consumers).map((a) => ({ ...a, name: shortName(a.id) })) : []
    : scope && !flat ? withPorts(entries, projected.consumers, placeOf) : entries
  const needle = query.trim().toLowerCase()
  const hits = searchHits({ needle, flat, show, graph, entries, scope, leavesOf, assets, byId, byKind })
  const stateOf = (entry: GraphEntry): GraphState => {
    const memberIds = leaves(entry.id)
    const liveStatus = live[entry.id]
    const health = entry.kind === 'group'
      ? groupHealth(rollup?.entries.get(entry.id))
      : liveStatus
        ? liveGraphState(liveStatus)
        : memberHealth(byId.get(entry.id))
    const pending = memberIds.some((id) => !byId.has(id)) && !statesFailed
    return pending && health.kind !== 'run' ? PENDING_GRAPH_STATE : health
  }
  const visualStates = Object.fromEntries((graph ? drawn : [...entries, ...hits]).map((entry) => [entry.id, stateOf(entry)]))
  const scopeIds = scope ? leaves(scope) : assets.map((a) => a.id)
  const inScope = new Set(scopeIds)
  const scopedStates = states.filter((s) => inScope.has(s.id))
  const rankOf = (entry: GraphEntry): EntryRank => {
    const ids = leaves(entry.id)
    const counts = rollup?.entries.get(entry.id)
    return { name: entry.name ?? entry.id, failed: counts?.health.failing ?? 0, stale: counts ? needsAttention(counts) : 0, lastSuccessAt: scopeAge(ids, byId).lastSuccessAt }
  }
  const sorted = entries.map((entry) => ({ entry, rank: rankOf(entry) })).sort((a, b) => compareEntries(params.sort, a.rank, b.rank)).map(({ entry }) => entry)
  // A search lists what matches by its own name: a group because of its name, never because a
  // member deep inside it (or its output) matches; that member is listed itself, with its path.
  const visibleEntries = [
    ...sorted.filter((entry) => !needle || (entry.name ?? entry.id).toLowerCase().includes(needle)),
    ...hits,
  ]
  return {
    assets, byId, leavesOf, sole, scope, entries, external: projected.external, tooBig, lineageOf, drawn, hits,
    consumers, visualStates, scopeIds, scopedStates, visibleEntries, isHit: new Set(hits.map((h) => h.id)), pathOf, placeOf, leaves,
  }
}

interface SearchInput {
  needle: string
  flat: boolean
  show: GroupedShow
  graph: boolean
  entries: readonly GraphEntry[]
  scope: string | null
  leavesOf: ReadonlyMap<string, string[]>
  assets: readonly Pick<ListNode, 'id'>[]
  byId: ReadonlyMap<string, NodeState>
  byKind: ReadonlyMap<string, ListNode['kind']>
}

/**
 * A search reaches every node in scope, not just this level: a node whose name matches, deep in
 * a nested group, is listed with its path, what needs attention first (a failed last attempt,
 * then a state other than cached), then by name; at most `SEARCH_LIMIT`. Nothing in a flat
 * view, the failures list or the graph.
 */
export function searchHits({ needle, flat, show, graph, entries, scope, leavesOf, assets, byId, byKind }: SearchInput): GraphEntry[] {
  if (!needle || flat || show === 'failures' || graph) return []
  const level = new Set(entries.map((e) => e.id))
  const ids = scope ? leavesOf.get(scope) ?? [] : assets.map((a) => a.id)
  const rank = (id: string) => {
    const box = memberHealth(byId.get(id))
    return isFailed(box) ? 0 : box.kind === 'cache' && box.state !== 'cached' ? 1 : 2
  }
  return ids
    .filter((id) => !level.has(id) && shortName(id).toLowerCase().includes(needle))
    .map((id) => ({ id, rank: rank(id), name: shortName(id) }))
    .sort((a, b) => a.rank - b.rank || NAME_ORDER.compare(a.name, b.name))
    .slice(0, SEARCH_LIMIT)
    .map(({ id }) => ({ id, kind: byKind.get(id) ?? 'asset', inputs: [], name: shortName(id) }))
}

/** The failures of the list matching the search. */
export function visibleFailures(list: readonly Failure[], query: string): Failure[] {
  const needle = query.trim().toLowerCase()
  return needle ? list.filter((f) => `${f.id} ${f.headline}`.toLowerCase().includes(needle)) : [...list]
}

/** The root of the breadcrumb: the open pipeline's file, or every pipeline. */
export function rootName(pipeline: string | null): string {
  return pipeline ? pipelineName(pipeline) : 'All pipelines'
}

/** The groups under `scope` at any depth (every group of the tree for the root). */
export function groupsUnder(scope: string | null, tree: GroupTree): number {
  if (scope === null) return tree.groups.size
  let n = 0
  const stack = [...(tree.groups.get(scope)?.members ?? [])]
  while (stack.length) {
    const group = tree.groups.get(stack.pop()!)
    if (!group) continue
    n += 1
    stack.push(...group.members)
  }
  return n
}

/**
 * What the header counts, named: the nodes in scope by kind, and the groups they are organized
 * in at any depth. `541 assets · 82 tasks · 51 sensors in 19 groups`. Checks are tasks here, as
 * the sidebar counts them; how many rows the table lists is the toolbar's `N shown`.
 */
export function scopeCountsLabel(ids: readonly string[], kindOf: ReadonlyMap<string, ListNode['kind']>, groups: number): string {
  const counts = { asset: 0, task: 0, sensor: 0 }
  for (const id of ids) counts[kindOf.get(id) ?? 'asset'] += 1
  const kinds = (['asset', 'task', 'sensor'] as const).filter((k) => counts[k] > 0).map((k) => plural(counts[k], k))
  const nodes = kinds.length ? kinds.join(' · ') : '0 nodes'
  return groups > 0 ? `${nodes} in ${plural(groups, 'group')}` : nodes
}

/**
 * The rows in the order they were shown (`previous`, ids), new rows after them in `sorted`'s
 * order: while the pointer is over a table its rows do not move when states arrive or change
 * (a group's Checking… resolving to failing). The table takes the fresh order when the pointer
 * leaves, or at once when the sort, the scope or the search changes.
 */
export function holdOrder<T extends { id: string }>(previous: readonly string[] | null, sorted: readonly T[]): T[] {
  if (!previous) return [...sorted]
  const rank = new Map(previous.map((id, i) => [id, i]))
  const kept = sorted.filter((e) => rank.has(e.id)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!)
  return [...kept, ...sorted.filter((e) => !rank.has(e.id))]
}

/** Whether two id lists are the same, in the same order. */
export function sameOrder(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i])
}
