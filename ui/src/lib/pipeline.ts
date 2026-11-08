import type { ListNode, NodeKind, NodeState } from './types'
import { fileDisplay, type AssetRow, type FileDisplay } from './assetTable'

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

/** A pipeline (source file) as the sidebar shows it, for the nodes of one kind. */
export interface PipelineSummary extends Pipeline {
  /** Its nodes of the counted kind, from graph metadata: known before any state is. */
  total: number
  /**
   * Its one state, exactly as the file's header in the table shows it (`fileDisplay`); null
   * until every counted node of it has a state (a part says nothing of the whole) and nothing
   * of it is running.
   */
  shown: FileDisplay | null
  /** How many of them a run is working on now. */
  running: number
}

/**
 * One line per source file of the project, in file order: how many of its nodes are of `kind`
 * (the page's: Assets counts assets), and their one state from `rows` (`buildRows` over the
 * loaded states of that kind). A file with none of that kind stays listed, at 0, so the list
 * does not move from page to page.
 */
export function pipelineSummaries(
  rows: readonly AssetRow[],
  listed: readonly Pick<ListNode, 'id' | 'kind'>[],
  kind: NodeKind,
): PipelineSummary[] {
  const rowById = new Map(rows.map((r) => [r.id, r]))
  const byFile = new Map<string, { total: number; rows: AssetRow[]; known: boolean }>()
  for (const node of listed) {
    const file = sourceFile(node.id)
    const entry = byFile.get(file) ?? { total: 0, rows: [], known: true }
    byFile.set(file, entry)
    if (node.kind !== kind) continue
    entry.total += 1
    const row = rowById.get(node.id)
    if (row) entry.rows.push(row)
    else entry.known = false
  }
  return [...byFile]
    .map(([file, { total, rows, known }]) => {
      const running = rows.filter((r) => r.running).length
      return { file, name: pipelineName(file), total, running, shown: known || running > 0 ? fileDisplay(rows) : null }
    })
    .sort((a, b) => a.file.localeCompare(b.file))
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
