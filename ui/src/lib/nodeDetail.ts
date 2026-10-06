/**
 * Node detail panel — pure helpers over data the server already returns
 * (`GET /state` for the node and its neighbours, `GET /assets/{id}` for its
 * run history). Nothing here fetches.
 */

import { formatSeconds } from './assetTable'
import type { AssetRunEntry, NodeState } from './types'

/** Nodes that take `id` as an input, sorted by name. */
export function downstreamOf(id: string, nodes: NodeState[]): NodeState[] {
  return nodes
    .filter((n) => n.inputs.includes(id))
    .sort((a, b) => a.name.localeCompare(b.name))
}

export interface HistoryBar {
  createdAt: string
  /** Height relative to the slowest run shown (failures with no duration: full). */
  heightPct: number
  failed: boolean
  /** Tooltip text. */
  label: string
}

/** Bars for the recent-runs chart, oldest first (the API returns newest first). */
export function historyBars(runs: AssetRunEntry[]): HistoryBar[] {
  const max = Math.max(0, ...runs.map((r) => r.elapsed_seconds ?? 0))
  return [...runs].reverse().map((r) => {
    const failed = r.status !== 'success'
    const e = r.elapsed_seconds
    return {
      createdAt: r.created_at,
      heightPct: e === null || max === 0 ? 100 : Math.max(4, Math.round((e / max) * 100)),
      failed,
      label: e === null ? r.status : `${r.status} · ${formatSeconds(e)}`,
    }
  })
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i += 1
  }
  return `${v.toFixed(1)} ${units[i]}`
}

/** The first 12 hex digits of a run hash — enough to tell versions apart. */
export function shortHash(h: string): string {
  return h.slice(0, 12)
}
