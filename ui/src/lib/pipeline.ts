import type { ListNode, NodeState } from './types'
import { SEVERITIES, severityOf, type Severity } from './assetTable'

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

/** Distinct pipelines actually being served, derived from the asset graph. */
export function pipelinesFromAssets(assets: ListNode[]): Pipeline[] {
  const seen = new Map<string, Pipeline>()
  for (const a of assets) {
    const file = sourceFile(a.id)
    if (!seen.has(file)) seen.set(file, { name: pipelineName(file), file })
  }
  return [...seen.values()]
}

/** A pipeline (source file) as the sidebar shows it. */
export interface PipelineSummary extends Pipeline {
  /** Nodes defined in this file. */
  total: number
  /** The most urgent state among them (what its status dot shows). */
  worst: Severity
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
    const severity = n ? severityOf(n) : 'unknown'
    const p = byFile.get(file)
    const running = n?.running ? 1 : 0
    if (!p) {
      byFile.set(file, { file, name: pipelineName(file), total: 1, worst: severity, running })
    } else {
      p.total += 1
      p.running += running
      if (SEVERITIES.indexOf(severity) < SEVERITIES.indexOf(p.worst)) p.worst = severity
    }
  }
  return [...byFile.values()].sort((a, b) => a.file.localeCompare(b.file))
}

/** Whether node `id` belongs to the selected pipeline (`null` = all). */
export function inPipeline(id: string, file: string | null): boolean {
  return file === null || sourceFile(id) === file
}
