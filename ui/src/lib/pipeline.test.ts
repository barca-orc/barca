import { describe, expect, it } from 'vitest'
import type { NodeState } from './types'
import { fileShown, inPipeline, pipelineSummaries, sourceFile } from './pipeline'
import { fileSums, causeCounts } from './testdata/rollup'
import type { FileRollup } from './generated/FileRollup'

type Listed = Pick<NodeState, 'id' | 'kind'>
const node = (id: string, kind: NodeState['kind'] = 'asset'): Listed => ({ id, kind })

describe('pipelineSummaries', () => {
  const nodes = [
    node('pipeline/p02_reconcile.py:grid'),
    node('pipeline/p01_dims.py:dim_ppg'),
    node('pipeline/p01_dims.py:dim_item'),
    node('pipeline/p02_reconcile.py:check'),
    node('other/p01_dims.py:clash'),
  ]
  // `files` of `GET /state/rollup`, as the server sums them.
  const served = (...files: FileRollup[]) => new Map(files.map((f) => [f.file, f]))
  const files = served(
    fileSums('other/p01_dims.py', 'ok'),
    fileSums('pipeline/p01_dims.py', 'behind', { nodes: 2, causes: causeCounts({ catching_up: 2 }) }),
    fileSums('pipeline/p02_reconcile.py', 'failing', { nodes: 2, causes: causeCounts({ failing: 2 }) }),
  )

  it('one entry per source file, in file order, with its node count', () => {
    expect(pipelineSummaries(files, nodes, 'asset').map((p) => [p.file, p.total])).toEqual([
      ['other/p01_dims.py', 1],
      ['pipeline/p01_dims.py', 2],
      ['pipeline/p02_reconcile.py', 2],
    ])
  })

  it('names are the file name; files are what tell same-named pipelines apart', () => {
    expect(pipelineSummaries(files, nodes, 'asset').filter((p) => p.name === 'p01_dims.py').map((p) => p.file)).toEqual([
      'other/p01_dims.py',
      'pipeline/p01_dims.py',
    ])
  })

  it('shows each file in the health map\'s words: the first cause its sums in the rollup have a node of, fresh, or its run work', () => {
    const byFile = Object.fromEntries(pipelineSummaries(files, nodes, 'asset').map((p) => [p.file, p.shown?.label]))
    expect(byFile).toEqual({ 'other/p01_dims.py': 'fresh', 'pipeline/p01_dims.py': 'catching up', 'pipeline/p02_reconcile.py': 'failing' })
    expect(fileShown(fileSums('a.py', 'failing', { running: 2, causes: causeCounts({ failing: 1 }) }))).toEqual({ glyph: 'running', label: '2 running', part: null, live: true })
    // A file a failure holds reads failing, as the map counts it; the first cause in the served order leads.
    expect(fileShown(fileSums('a.py', 'blocked', { causes: causeCounts({ failing: 1 }) }))).toMatchObject({ glyph: 'failed', label: 'failing', part: 'failing', live: false })
    expect(fileShown(fileSums('a.py', 'behind', { nodes: 5, causes: causeCounts({ catching_up: 3, overdue: 1 }) }))).toMatchObject({ label: 'overdue', part: 'overdue' })
  })

  it('counts only the nodes of the kind, and keeps a file with none of it at 0 and without a state', () => {
    const mixed = [...nodes, node('other/p01_dims.py:ping', 'task')]
    expect(pipelineSummaries(files, mixed, 'asset').find((p) => p.file === 'other/p01_dims.py')?.total).toBe(1)
    const tasks = pipelineSummaries(files, mixed, 'task')
    expect(tasks.map((p) => [p.file, p.total])).toEqual([
      ['other/p01_dims.py', 1],
      ['pipeline/p01_dims.py', 0],
      ['pipeline/p02_reconcile.py', 0],
    ])
    expect(tasks.find((p) => p.total === 0)?.shown).toBeNull()
  })

  it('counts metadata before the rollup arrives, and says no state until it does', () => {
    const pending = pipelineSummaries(null, nodes, 'asset')
    expect(pending.map((p) => p.total)).toEqual([1, 2, 2])
    expect(pending.every((p) => p.shown === null)).toBe(true)
    // A file the rollup has no sums for (out of its scope) has no state either.
    const partial = pipelineSummaries(served(fileSums('pipeline/p02_reconcile.py', 'failing', { causes: causeCounts({ failing: 1 }) })), nodes, 'asset')
    expect(partial.find((p) => p.file === 'pipeline/p02_reconcile.py')?.shown?.label).toBe('failing')
    expect(partial.find((p) => p.file === 'pipeline/p01_dims.py')?.shown).toBeNull()
  })
})

describe('inPipeline', () => {
  it('matches a node by its exact source file', () => {
    expect(inPipeline('pipeline/p01_dims.py:dim_ppg', 'pipeline/p01_dims.py')).toBe(true)
    expect(inPipeline('other/p01_dims.py:clash', 'pipeline/p01_dims.py')).toBe(false)
  })
  it('no selection keeps everything', () => {
    expect(inPipeline('pipeline/p01_dims.py:dim_ppg', null)).toBe(true)
  })
  it('sourceFile splits on the last colon', () => {
    expect(sourceFile('pipeline/p01_dims.py:dim_ppg')).toBe('pipeline/p01_dims.py')
  })
})
