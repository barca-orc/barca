/**
 * The command palette (⌘K) as a program (`lib/program.ts`): an index over what the UI already
 * loaded, one search over it, and the keyboard's place in the results. Pure;
 * `layouts/CommandPalette.tsx` runs it.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import { pipelineName, sourceFile } from './pipeline'
import { only, type Next } from './program'
import { formatAgo } from './time'
import type { ListNode, NodeGroup, PartitionKeyRow, RunSummary } from './types'

/** What a result does: go somewhere, or one of the page's actions. */
export type PaletteTarget = { type: 'go'; to: string } | { type: 'run' } | { type: 'open_graph' }

export interface PaletteItem {
  key: string
  section: 'Actions' | 'Nodes' | 'Groups' | 'Files' | 'Runs' | 'Keys'
  label: string
  detail: string
  target: PaletteTarget
}

export const PALETTE_LIMIT = 50
/** How many partitioned nodes a key query asks, and how many keys each may answer with. */
export const KEY_SEARCH_NODES = 8
export const KEYS_PER_NODE = 5
const NAMES = new Intl.Collator(undefined, { numeric: true })

const link = (path: string, params: Record<string, string>) => `${path}?${new URLSearchParams(params)}`

function nodeItem(a: Pick<ListNode, 'id' | 'kind'>): PaletteItem {
  const pipeline = sourceFile(a.id)
  const path = match(a.kind).with('asset', () => '/assets').with('task', () => '/tasks').with('sensor', () => '/sensors').exhaustive()
  return { key: a.id, section: 'Nodes', label: shortName(a.id), detail: `${a.kind} · ${pipelineName(pipeline)}`, target: { type: 'go', to: link(path, { pipeline, node: a.id }) } }
}

/** `group:billing.py:raw` → `billing.py`. */
const groupFile = (id: string) => id.split(':')[1] ?? ''

/**
 * Everything the palette can jump to, in the order an empty search shows it: what the project
 * is made of (files, groups, nodes) before its history (runs), which a run id prefix still
 * reaches first.
 */
export function paletteIndex(
  assets: readonly Pick<ListNode, 'id' | 'kind'>[],
  groups: readonly Pick<NodeGroup, 'id' | 'name'>[],
  runs: readonly Pick<RunSummary, 'id' | 'target' | 'status'>[],
): PaletteItem[] {
  const files = [...new Set(assets.map((a) => sourceFile(a.id)))].sort(NAMES.compare)
  return [
    ...files.map((file): PaletteItem => ({ key: `file:${file}`, section: 'Files', label: pipelineName(file), detail: file, target: { type: 'go', to: link('/assets', { pipeline: file }) } })),
    ...groups.map((g): PaletteItem => ({ key: g.id, section: 'Groups', label: g.name, detail: pipelineName(groupFile(g.id)), target: { type: 'go', to: link('/assets', { pipeline: groupFile(g.id), group: g.id }) } })),
    ...assets.map(nodeItem),
    ...runs.map((r): PaletteItem => ({ key: `run:${r.id}`, section: 'Runs', label: r.id, detail: `${r.target ?? 'all assets'} · ${r.status}`, target: { type: 'go', to: link('/runs', { run: r.id }) } })),
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

/** `account=ra-02541` → `ra-02541`, what the node panel's key search is pre-filled with; a key of several dimensions stays whole. */
const keyValue = (key: string) => /^[^=,]+=([^,]*)$/.exec(key)?.[1] ?? key

/** One result per key found, failed keys first so the problem is on top, then in node and key order. */
export function keyItems(hits: readonly KeyHits[], nowMs: number): PaletteItem[] {
  const items = hits.flatMap(({ node, keys }) =>
    keys.map((k): PaletteItem => ({
      key: `key:${node}:${k.key}`,
      section: 'Keys',
      label: `${shortName(node)} · ${k.key}`,
      detail: `${k.status} ${formatAgo(k.created_at, nowMs)}`,
      target: { type: 'go', to: link('/assets', { pipeline: sourceFile(node), node, key: keyValue(k.key) }) },
    })),
  )
  const failed = (item: PaletteItem) => (item.detail.startsWith('failed') ? 0 : 1)
  return items.sort((a, b) => failed(a) - failed(b))
}

/** The page's actions, first in every list. */
export function paletteActions(runLabel: string | null, canGraph: boolean): PaletteItem[] {
  return [
    ...(runLabel ? [{ key: 'action:run', section: 'Actions' as const, label: runLabel, detail: 'action', target: { type: 'run' as const } }] : []),
    ...(canGraph ? [{ key: 'action:graph', section: 'Actions' as const, label: 'Open graph', detail: 'action', target: { type: 'open_graph' as const } }] : []),
  ]
}

/** Name prefix, then name, then detail; a tie stays in index order. */
function rank(item: PaletteItem, needle: string): number | null {
  const label = item.label.toLowerCase()
  if (label.startsWith(needle)) return 0
  if (label.includes(needle)) return 1
  return item.detail.toLowerCase().includes(needle) ? 2 : null
}

/** The index's matches, then the keys the server found (`keyItems`); at most `limit` in all. */
export function searchPalette(items: readonly PaletteItem[], query: string, keys: readonly PaletteItem[] = [], limit = PALETTE_LIMIT): PaletteItem[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return items.slice(0, limit)
  const found = items
    .flatMap((item) => {
      const r = rank(item, needle)
      return r === null ? [] : [{ item, r }]
    })
    .sort((a, b) => a.r - b.r)
    .map(({ item }) => item)
  return [...found, ...keys].slice(0, limit)
}

export interface PaletteModel {
  query: string
  /** The highlighted result. */
  active: number
}

export type PaletteMsg = { type: 'query_changed'; value: string } | { type: 'moved'; by: number; count: number }

export const INITIAL_PALETTE: PaletteModel = { query: '', active: 0 }

export function updatePalette(model: PaletteModel, msg: PaletteMsg): Next<PaletteModel, never> {
  return match<PaletteMsg, Next<PaletteModel, never>>(msg)
    .with({ type: 'query_changed' }, ({ value }) => only({ query: value, active: 0 }))
    .with({ type: 'moved' }, ({ by, count }) => only({ ...model, active: count ? (model.active + by + count) % count : 0 }))
    .exhaustive()
}
