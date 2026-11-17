import type { FileRollup } from './generated/FileRollup'
import type { Glyph } from './glyph'
import { leadingPart, mapPartGlyph, mapPartLabel, type MapPart } from './causeDisplay'
import type { ListNode, NodeKind } from './types'

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

/** How a file reads as one: a glyph and its word, as its header and the sidebar show it. */
export interface FileDisplay {
  glyph: Glyph
  label: string
  /** The cause (or fresh) the word is of, whose colour marks the line; null while a run leads. */
  part: MapPart | null
  /** A run is working on some of its nodes: the running glyph turns. */
  live: boolean
}

/**
 * A file's one state, for its header in the table and its line in the sidebar, in the health
 * map's words: the words of its sums in the rollup (`files` of `GET /state/rollup`, summed in
 * `barca-core`). A run working on some of its nodes leads; else the first cause, in the served
 * order, that the file has a node of (`causes`), or `fresh` when it has none. Nothing here
 * counts or ranks.
 */
export function fileShown(sums: Pick<FileRollup, 'running' | 'causes'>): FileDisplay {
  if (sums.running > 0) return { glyph: 'running', label: `${sums.running.toLocaleString()} running`, part: null, live: true }
  const part = leadingPart(sums.causes)
  return { glyph: mapPartGlyph(part), label: mapPartLabel(part), part, live: false }
}

/** A pipeline (source file) as the sidebar shows it, for the nodes of one kind. */
export interface PipelineSummary extends Pipeline {
  /** Its nodes of the counted kind, from graph metadata: known before any state is. */
  total: number
  /** Its one state (`fileShown`); null until the rollup has loaded, or for a file it has no node of. */
  shown: FileDisplay | null
  /** How many of its nodes a run is working on now. */
  running: number
}

/**
 * One line per source file of the project, in file order: how many of its nodes are of `kind`
 * (the page's: Assets counts assets), from the graph (`GET /assets`), and its one state from
 * the rollup's `files` (null before it loads). A file with none of that kind is here at 0; the
 * sidebar leaves it out (`listedPipelines`).
 */
export function pipelineSummaries(
  files: ReadonlyMap<string, FileRollup> | null,
  listed: readonly Pick<ListNode, 'id' | 'kind'>[],
  kind: NodeKind,
): PipelineSummary[] {
  const totals = new Map<string, number>()
  for (const node of listed) {
    const file = sourceFile(node.id)
    totals.set(file, (totals.get(file) ?? 0) + (node.kind === kind ? 1 : 0))
  }
  return [...totals]
    .map(([file, total]) => {
      const sums = total > 0 ? files?.get(file) : undefined
      return { file, name: pipelineName(file), total, running: sums?.running ?? 0, shown: sums ? fileShown(sums) : null }
    })
    .sort((a, b) => a.file.localeCompare(b.file))
}

/** Whether node `id` belongs to the selected pipeline (`null` = all). */
export function inPipeline(id: string, file: string | null): boolean {
  return file === null || sourceFile(id) === file
}
