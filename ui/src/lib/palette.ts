/**
 * The command palette (⌘K) as a program (`lib/program.ts`): an index over what the UI already
 * loaded, one search over it, and the keyboard's place in the results. Pure;
 * `layouts/CommandPalette.tsx` runs it.
 */

import { keysHref } from './keysPage'
import { match } from 'ts-pattern'
import { shortName } from './graph'
import { graphState, isLive, type GraphState } from './graphState'
import { groupHealth, type GroupScope } from './groups'
import type { Glyph } from './glyph'
import { pipelineName, sourceFile } from './pipeline'
import { only, type Next } from './program'
import { isFailure, runGlyph, runLabel as runWord, runStatus } from './runs'
import { statusMeta } from './status'
import { formatAgo } from './time'
import type { Tone } from './tone'
import type { ListNode, NodeGroup, NodeState, PartitionKeyRow, RunSummary } from './types'

/** What a result does: go somewhere, or one of the page's actions. */
type PaletteTarget = { type: 'go'; to: string } | { type: 'run' } | { type: 'open_graph' }

/** A result's state, drawn as its glyph before its name: a failing node and a fresh one never look alike. */
export interface PaletteState {
  glyph: Glyph
  /** The state in words: the glyph's tooltip and its name for a screen reader. */
  label: string
  /** A run is working on it: the running glyph turns. */
  live: boolean
  /** How urgent: what ranks first among equal matches. */
  tone: Tone
}

export interface PaletteItem {
  key: string
  section: 'Actions' | 'Nodes' | 'Groups' | 'Files' | 'Runs' | 'Keys'
  label: string
  detail: string
  target: PaletteTarget
  /** Null for an action, and for a node, group or file whose state is not loaded. */
  state: PaletteState | null
}

/** What the index knows of the project's state: every node's row of `GET /state`, and each group's leaves. */
export interface PaletteStates {
  byId: Map<string, NodeState>
  /** The project's rollup entries by id (`GET /state/rollup`): a group's state, a file's. */
  entries: ReadonlyMap<string, GroupScope>
  /** A nested group's ancestors (`Ads › Staging`), so groups of one name tell apart. */
  placeOf?: ReadonlyMap<string, string>
}

const NO_STATES: PaletteStates = { byId: new Map(), entries: new Map() }

/** The most urgent first: what a search lists first among results that match equally well. */
const URGENCY: Record<Tone, number> = { failed: 0, warning: 1, running: 2, queued: 3, neutral: 4, success: 5 }

function shown(box: GraphState): PaletteState {
  return { glyph: box.glyph, label: box.label, live: isLive(box), tone: box.tone }
}

/**
 * The state of a scope (a group, or a file whose nodes are in no group): its entry of the
 * rollup, null when the rollup has none (not loaded, or a file whose nodes sit in groups).
 */
function scopeState(id: string, states: PaletteStates): PaletteState | null {
  const entry = states.entries.get(id)
  return entry ? shown(groupHealth(entry)) : null
}

const PALETTE_LIMIT = 50
/** How many partitioned nodes a key query asks, and how many keys each may answer with. */
export const KEY_SEARCH_NODES = 8
export const KEYS_PER_NODE = 5
const NAMES = new Intl.Collator(undefined, { numeric: true })

const link = (path: string, params: Record<string, string>) => `${path}?${new URLSearchParams(params)}`

function nodeItem(a: Pick<ListNode, 'id' | 'kind'>, states: PaletteStates): PaletteItem {
  const pipeline = sourceFile(a.id)
  const path = match(a.kind).with('asset', () => '/assets').with('task', () => '/tasks').with('sensor', () => '/sensors').exhaustive()
  const node = states.byId.get(a.id)
  return { key: a.id, section: 'Nodes', label: shortName(a.id), detail: `${a.kind} · ${pipelineName(pipeline)}`, target: { type: 'go', to: link(path, { pipeline, node: a.id }) }, state: node ? shown(graphState(node)) : null }
}

function runState(status: string): PaletteState {
  const kind = runStatus(status)
  return { glyph: runGlyph(status), label: runWord(status), live: statusMeta(kind).live, tone: statusMeta(kind).tone }
}

/** `group:billing.py:raw` → `billing.py`. */
const groupFile = (id: string) => id.split(':')[1] ?? ''

/**
 * Everything the palette can jump to, in the order an empty search shows it: what the project
 * is made of (files, groups, nodes) before its history (runs), which a run id prefix still
 * reaches first. Each carries its state, as the lists show it: a node's, the worst of a group's
 * or a file's nodes (once all of them are known), a run's status.
 */
export function paletteIndex(
  assets: readonly Pick<ListNode, 'id' | 'kind'>[],
  groups: readonly Pick<NodeGroup, 'id' | 'name'>[],
  runs: readonly Pick<RunSummary, 'run_id' | 'target' | 'status'>[],
  states: PaletteStates = NO_STATES,
): PaletteItem[] {
  const byFile = new Map<string, string[]>()
  for (const a of assets) {
    const file = sourceFile(a.id)
    const ids = byFile.get(file)
    if (ids) ids.push(a.id)
    else byFile.set(file, [a.id])
  }
  const files = [...byFile.keys()].sort(NAMES.compare)
  return [
    ...files.map((file): PaletteItem => ({ key: `file:${file}`, section: 'Files', label: pipelineName(file), detail: file, target: { type: 'go', to: link('/assets', { pipeline: file }) }, state: groups.some((g) => groupFile(g.id) === file) ? null : scopeState(`file:${file}`, states) })),
    ...groups.map((g): PaletteItem => ({ key: g.id, section: 'Groups', label: g.name, detail: [states.placeOf?.get(g.id), pipelineName(groupFile(g.id))].filter(Boolean).join(' · '), target: { type: 'go', to: link('/assets', { pipeline: groupFile(g.id), group: g.id }) }, state: scopeState(g.id, states) })),
    ...assets.map((a) => nodeItem(a, states)),
    ...runs.map((r): PaletteItem => ({ key: `run:${r.run_id}`, section: 'Runs', label: r.run_id, detail: `${r.target ?? 'all assets'} · ${runWord(r.status)}`, target: { type: 'go', to: link('/runs', { run: r.run_id }) }, state: runState(r.status) })),
  ]
}

/**
 * Whether `query` may name a partition key, and so is worth asking the server: at least two
 * characters with a digit, `=`, `-`, `_` or `/` (`ra-02541`, `account=ra-02541`, `2026-10-30`).
 * A plain word is a node, group or file name, which the index already has.
 */
export function isKeyQuery(query: string): boolean {
  const q = query.trim()
  return q.length >= 2 && /[\d=_/-]/.test(q)
}

/** The keys one partitioned node answered a key query with. */
export interface KeyHits {
  node: string
  keys: readonly PartitionKeyRow[]
}

/** One result per key found, failed keys first so the problem is on top, then in node and key order. */
export function keyItems(hits: readonly KeyHits[], nowMs: number): PaletteItem[] {
  const items = hits.flatMap(({ node, keys }) =>
    keys.map((k): PaletteItem => ({
      key: `key:${node}:${k.key}`,
      section: 'Keys',
      label: `${shortName(node)} · ${k.key}`,
      detail: `${k.status} ${formatAgo(k.created_at, nowMs)}`,
      // The key explorer, at the key: its data, its error and the same key along its chain.
      target: { type: 'go', to: keysHref(node, { key: k.key }) },
      state: isFailure(k.status)
        ? { glyph: 'failed', label: 'failed', live: false, tone: 'failed' }
        : { glyph: 'fresh', label: k.status, live: false, tone: 'success' },
    })),
  )
  return items.sort((a, b) => urgency(a) - urgency(b))
}

/** An item's place among equal matches: failing first, then what needs attention; an action or an unknown state in the middle. */
function urgency(item: PaletteItem): number {
  return item.state ? URGENCY[item.state.tone] : URGENCY.neutral
}

/** The page's actions, first in every list. */
export function paletteActions(runLabel: string | null, canGraph: boolean): PaletteItem[] {
  return [
    ...(runLabel ? [{ key: 'action:run', section: 'Actions' as const, label: runLabel, detail: 'action', target: { type: 'run' as const }, state: null }] : []),
    ...(canGraph ? [{ key: 'action:graph', section: 'Actions' as const, label: 'Open graph', detail: 'action', target: { type: 'open_graph' as const }, state: null }] : []),
  ]
}

/** Name prefix, then name, then detail; a tie stays in index order. */
function rank(item: PaletteItem, needle: string): number | null {
  const label = item.label.toLowerCase()
  if (label.startsWith(needle)) return 0
  if (label.includes(needle)) return 1
  return item.detail.toLowerCase().includes(needle) ? 2 : null
}

/**
 * The index's matches, then the keys the server found (`keyItems`); at most `limit` in all.
 * Actions first, then by how well each matches and, among equal matches, failing before
 * healthy (`URGENCY`); a tie stays in index order.
 */
export function searchPalette(items: readonly PaletteItem[], query: string, keys: readonly PaletteItem[] = [], limit = PALETTE_LIMIT): PaletteItem[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return items.slice(0, limit)
  const found = items
    .flatMap((item) => {
      const r = rank(item, needle)
      return r === null ? [] : [{ item, r }]
    })
    .sort((a, b) => Number(b.item.section === 'Actions') - Number(a.item.section === 'Actions') || a.r - b.r || urgency(a.item) - urgency(b.item))
    .map(({ item }) => item)
  return [...found, ...keys].slice(0, limit)
}

interface PaletteModel {
  query: string
  /** The highlighted result. */
  active: number
}

type PaletteMsg = { type: 'query_changed'; value: string } | { type: 'moved'; by: number; count: number }

export const INITIAL_PALETTE: PaletteModel = { query: '', active: 0 }

export function updatePalette(model: PaletteModel, msg: PaletteMsg): Next<PaletteModel, never> {
  return match<PaletteMsg, Next<PaletteModel, never>>(msg)
    .with({ type: 'query_changed' }, ({ value }) => only({ query: value, active: 0 }))
    .with({ type: 'moved' }, ({ by, count }) => only({ ...model, active: count ? (model.active + by + count) % count : 0 }))
    .exhaustive()
}
