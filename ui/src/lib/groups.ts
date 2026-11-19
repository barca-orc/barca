/** Organizational metadata, independent of execution. */
import type { ListNode, NodeState } from './types'
import { plural } from './plural'
import { shortName, type GraphEntry } from './graph'
import { causeDisplay, causeParts, leadsRunWork, mapPartGlyph, mapPartLabel } from './causeDisplay'
import type { GraphState, GroupPart } from './graphState'
import { isCheck } from './status'

import type { NodeGroup } from './types'
import type { RollupEntry } from './generated/RollupEntry'
import { formatSeconds } from './time'
export type { NodeGroup } from './types'

export interface GroupTree {
  groups: Map<string, NodeGroup>
  roots: string[]
  parents: Map<string, string>
}

/** Where the rollup puts an entry: all a tree needs of `GET /state/rollup`'s entries. */
export type Placement = ReadonlyMap<string, Pick<RollupEntry, 'parent'>>

/**
 * The organizational tree of the groups the parser found (`GET /groups`). A node or group
 * declared in several groups is counted by the rollup under one of them (`barca-core`'s
 * `rollup`, the entry's `parent`); with `placed` (the rollup's entries) each group lists the
 * members the rollup counts under it, so a list and its count are the same nodes. The other
 * groups that declare a member are its entry's `also_in`. Nothing is decided here: without
 * `placed` (the rollup has not loaded) the tree is the declarations as given.
 */
export function groupTree(metadata: NodeGroup[], placed?: Placement): GroupTree {
  const listed = placed
    ? metadata.map((g) => ({ ...g, members: g.members.filter((m) => (placed.get(m)?.parent ?? g.id) === g.id) }))
    : metadata
  const groups = new Map(listed.map(g => [g.id, g]))
  const parents = new Map<string, string>()
  for (const g of listed) for (const member of g.members) parents.set(member, g.id)
  return { groups, roots: listed.filter(g => !parents.has(g.id)).map(g => g.id), parents }
}

/** `also in reports, finance`: the other groups that declare an entry (`also_in`), by name; null for none. */
export function alsoInNote(alsoIn: readonly string[] | undefined, tree: GroupTree): string | null {
  if (!alsoIn || alsoIn.length === 0) return null
  return `also in ${alsoIn.map((id) => tree.groups.get(id)?.name ?? id).join(', ')}`
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

function memberName(id: string): string {
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
export type EntrySort = 'worst' | 'name'
export const ENTRY_SORTS: readonly EntrySort[] = ['worst', 'name']

export function parseEntrySort(raw: string | null): EntrySort {
  return ENTRY_SORTS.find((s) => s === raw) ?? 'worst'
}

/** What a row of a grouped table is ordered by. */
export interface EntryRank {
  name: string
  /** The entry's served `weight` (`GET /state/rollup`): its distance from fresh. 0 until loaded. */
  alarmWeight: number
  inHandWeight: number
}

const RANK_NAMES = new Intl.Collator(undefined, { numeric: true })

/**
 * Rank by the served actionable weight, then in-hand weight. Fresh nodes add neither.
 */
export function compareEntries(sort: EntrySort, a: EntryRank, b: EntryRank): number {
  const name = RANK_NAMES.compare(a.name, b.name)
  if (sort === 'name') return name
  return b.alarmWeight - a.alarmWeight || b.inHandWeight - a.inHandWeight || name
}

/** What a group's box reads of its rollup entry: its nodes by cause, the fresh ones, and run work. */
export type GroupScope = Pick<RollupEntry, 'causes' | 'weights' | 'running' | 'nodes'>

/**
 * A group's box, in the health map's words, from the group's entry of the rollup (`GET
 * /state/rollup`): failing nodes first, then run work, then each other cause in the served
 * order, then the fresh nodes (`weights.fresh`, one each). `parts` carries every part, for a
 * card to show the rest compactly, and the hint names them all, with how many nodes they are of.
 * A group with run work and no failure is a run box (it pulses); a failure inside a group that
 * is also running still leads. A group has no state of its own: this words its entry, and counts
 * nothing. No entry (the rollup has none for it, or has not loaded) is an empty box.
 */
export function groupHealth(scope: GroupScope | undefined): GraphState {
  const parts: GroupPart[] = scope
    ? causeParts(scope.causes).map((p) => ({ part: p.cause, glyph: causeDisplay(p.cause).glyph, tone: causeDisplay(p.cause).tone, count: p.count, word: p.label, text: `${p.count.toLocaleString()} ${p.label}` }))
    : []
  if (scope && scope.running > 0) {
    // Run work sits after a failure and before everything else.
    const lead = parts[0]?.part
    const at = lead !== undefined && lead !== 'fresh' && leadsRunWork(lead) ? 1 : 0
    parts.splice(at, 0, { glyph: 'running', tone: 'running', count: scope.running, word: 'running', text: `${scope.running.toLocaleString()} running` })
  }
  if (scope && scope.weights.fresh > 0) {
    parts.push({ part: 'fresh', glyph: mapPartGlyph('fresh'), tone: 'success', count: scope.weights.fresh, word: mapPartLabel('fresh'), text: `${scope.weights.fresh.toLocaleString()} ${mapPartLabel('fresh')}` })
  }
  const label = parts.map((p) => p.text).join(' · ')
  const hint = scope && label ? `${plural(scope.nodes, 'node')}: ${label}` : 'No members with a state.'
  const [first] = parts
  if (!first) return { kind: 'group', tone: 'neutral', glyph: 'pending', label: 'empty', hint, parts }
  if (first.part === undefined) {
    return { kind: 'run', tone: 'running', glyph: 'running', status: 'running', label: first.text, hint, parts }
  }
  return { kind: 'group', tone: first.tone, glyph: first.glyph, label: first.text, hint, part: first.part, parts }
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
      const kinds = leaves.map((leaf) => byId.get(leaf)).filter((node) => node !== undefined)
      return {
        id,
        kind: group ? 'group' : byId.get(id)!.kind,
        inputs: [...inputs],
        name: group?.name ?? names.get(id) ?? shortName(id),
        summary: contentsSummary(kinds),
        outputName: group ? memberName(groupOutput(id, tree)) : undefined,
      }
    })
  return { entries, external: [...external.keys()], consumers: external }
}

/**
 * What a group holds, by kind: `3 assets · 1 task`. A count of zero is left out, except that a
 * group of nothing reads `0 assets`; checks are tasks named `check__`/`validate__`.
 */
export function contentsSummary(nodes: readonly Pick<ListNode, 'id' | 'kind'>[]): string {
  const counts = { asset: 0, check: 0, task: 0, sensor: 0 }
  for (const node of nodes) counts[node.kind === 'task' ? (isCheck(node) ? 'check' : 'task') : node.kind] += 1
  const parts = (['asset', 'check', 'task', 'sensor'] as const)
    .filter((kind) => counts[kind] > 0)
    .map((kind) => plural(counts[kind], kind))
  return parts.length ? parts.join(' · ') : '0 assets'
}

/** `1 group`, `3 groups`. */
export { plural }

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

/** `850ms`, `2.3s`, `4.1m`: a run's time on a node, readable below a second too. */
function formatRunSeconds(s: number): string {
  return s < 1 ? `${Math.round(s * 1000)}ms` : formatSeconds(s)
}

/**
 * A leaf's Duration cell: what its newest run spent on it, added up over the keys the run
 * recorded (one key's row alone would be the batch's share for that key); `null` for a node
 * that never ran. A run whose keys all failed before finishing recorded no time, which is said
 * rather than shown as a dash.
 */
export function lastRunDuration(node: NodeState | undefined): { text: string; title?: string } | null {
  if (!node) return null
  const run = node.last_run
  if (!run) {
    const seconds = node.last_materialization?.elapsed_seconds
    return typeof seconds === 'number' ? { text: formatRunSeconds(seconds) } : node.last_materialization ? { text: '–' } : null
  }
  const keys = run.keys > 1
    ? `${run.keys.toLocaleString()} keys${run.calls ? ` in ${run.calls.toLocaleString()} batched ${run.calls === 1 ? 'call' : 'calls'}` : ''}`
    : null
  if (run.elapsed_seconds === null) {
    return {
      text: 'not recorded',
      title: `${run.failed.toLocaleString()} of ${run.keys.toLocaleString()} ${run.keys === 1 ? 'key' : 'keys'} failed before finishing; nothing recorded a time (run ${run.run_id})`,
    }
  }
  return {
    text: formatRunSeconds(run.elapsed_seconds),
    title: keys ? `Summed over ${keys}${run.failed ? `, ${run.failed.toLocaleString()} failed` : ''} (run ${run.run_id})` : `Run ${run.run_id}`,
  }
}

/**
 * Why a group's output node is in the state it is, when it did not fail itself: a group can be
 * failing while its output reads `unknown` or `stale`, because a member upstream of the output
 * failed. Names up to three of those members, then the server's own reason (`cache.detail`).
 */
export function groupOutputNote(output: Pick<NodeState, 'cache'>, failedUp: readonly string[]): string | null {
  const detail = output.cache.detail ? `${output.cache.detail[0]!.toUpperCase()}${output.cache.detail.slice(1)}` : ''
  if (failedUp.length === 0) return detail || null
  const names = failedUp.slice(0, 3).map(shortName).join(', ')
  const more = failedUp.length > 3 ? ` and ${failedUp.length - 3} more` : ''
  const waits = `Not up to date: ${plural(failedUp.length, 'failed node')} upstream of it (${names}${more}).`
  return detail ? `${waits} ${detail}${/[.!?]$/.test(detail) ? '' : '.'}` : waits
}
