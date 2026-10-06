import type { AssetSummary } from './types'

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
export function pipelinesFromAssets(assets: AssetSummary[]): Pipeline[] {
  const seen = new Map<string, Pipeline>()
  for (const a of assets) {
    const file = sourceFile(a.id)
    if (!seen.has(file)) seen.set(file, { name: pipelineName(file), file })
  }
  return [...seen.values()]
}
