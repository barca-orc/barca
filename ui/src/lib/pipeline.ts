import type { ListNode, NodeState } from './types'
import { cacheStateRank, lastAttemptFailed, type CacheState } from './cacheStateDisplay'

/**
 * Asset ids are "<source-file>:<name>" (e.g. "iris_project/assets.py:raw_data").
 * The source file is the part before the last colon.
 */
export function sourceFile(id: string): string {
  const idx = id.lastIndexOf(':')
  return idx === -1 ? id : id.slice(0, idx)
}

/**
 * The filename of a source file ("iris_project/assets.py" → "assets.py").
 * Used verbatim as the pipeline/title label — no invented project names.
 */
export function pipelineName(file: string): string {
  const parts = file.split('/')
  return parts[parts.length - 1] ?? file
}

export interface Pipeline {
  /** Display name. */
  name: string
  /** The full source-file prefix, unique per pipeline. */
  file: string
}

/** A pipeline (source file) as the sidebar shows it. */
export interface PipelineSummary extends Pipeline {
  /** Nodes defined in this file. */
  total: number
  /** The most urgent `cache.state` among them, in the CLI's word; `unknown` until one is loaded. */
  worst: CacheState
  /** How many of them failed their last attempt: a fact beside the state. */
  failed: number
  /** Every one of them has a loaded state: `worst` is the file's, not just a part's. */
  known: boolean
  /** How many of them a run is working on now. */
  running: number
}

/** Counts come from graph metadata; status may still be pending or cover only one cone. */
export function pipelineSummaries(
  nodes: NodeState[],
  assets: readonly Pick<ListNode, 'id'>[] = nodes,
): PipelineSummary[] {
  const states = new Map(nodes.map(node => [node.id, node]))
  const byFile = new Map<string, PipelineSummary>()
  for (const asset of assets) {
    const file = sourceFile(asset.id)
    const n = states.get(asset.id)
    const state: CacheState = n ? n.cache.state : 'unknown'
    const failed = n && lastAttemptFailed(n) ? 1 : 0
    const p = byFile.get(file)
    const running = n?.running ? 1 : 0
    if (!p) {
      byFile.set(file, { file, name: pipelineName(file), total: 1, worst: state, failed, known: n !== undefined, running })
    } else {
      p.total += 1
      p.running += running
      p.failed += failed
      p.known &&= n !== undefined
      if (cacheStateRank(state) < cacheStateRank(p.worst)) p.worst = state
    }
  }
  return [...byFile.values()].sort((a, b) => a.file.localeCompare(b.file))
}

/** Whether node `id` belongs to the selected pipeline (`null` = all). */
export function inPipeline(id: string, file: string | null): boolean {
  return file === null || sourceFile(id) === file
}

/** `fresh` rows, and every row of `older` that `fresh` does not have. */
export function mergeStates(older: NodeState[] | undefined, fresh: NodeState[] | undefined): NodeState[] {
  if (!older) return fresh ?? []
  if (!fresh) return older
  const ids = new Set(fresh.map((n) => n.id))
  return [...fresh, ...older.filter((n) => !ids.has(n.id))]
}
