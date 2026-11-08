import { describe, expect, it } from 'vitest'
import type { CacheStatus, NodeState } from './types'
import { inPipeline, pipelineSummaries, sourceFile } from './pipeline'
import { buildRows, fileDisplay, groupByFile } from './assetTable'

function node(id: string, state: CacheStatus['state'] = 'cached', failed = false): NodeState {
  return {
    id,
    name: id.split(':').pop()!,
    kind: 'asset',
    inputs: [],
    partitioned: false,
    cache: { state, reason: 'materialized', detail: '' },
    last_materialization: failed
      ? {
          status: 'failed',
          created_at: '2026-10-06 10:00:00',
          elapsed_seconds: null,
          run_hash: null,
          artifact: null,
          format: null,
          size_bytes: null,
          error: 'boom',
        }
      : null,
    shape: null,
    env: [],
    durations: null,
    next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0,
    running: null,
  }
}

describe('pipelineSummaries', () => {
  const nodes = [
    node('pipeline/p02_reconcile.py:grid'),
    node('pipeline/p01_dims.py:dim_ppg'),
    node('pipeline/p01_dims.py:dim_item', 'stale'),
    node('pipeline/p02_reconcile.py:check', 'always_runs', true),
    node('other/p01_dims.py:clash'),
  ]
  const summaries = (states: NodeState[], listed: Pick<NodeState, 'id' | 'kind'>[] = nodes, kind: NodeState['kind'] = 'asset') =>
    pipelineSummaries(buildRows(states.filter((n) => n.kind === kind), 0), listed, kind)

  it('one entry per source file, in file order, with its node count', () => {
    expect(summaries(nodes).map((p) => [p.file, p.total])).toEqual([
      ['other/p01_dims.py', 1],
      ['pipeline/p01_dims.py', 2],
      ['pipeline/p02_reconcile.py', 2],
    ])
  })

  it('names are the file name; files are what tell same-named pipelines apart', () => {
    expect(summaries(nodes).filter((p) => p.name === 'p01_dims.py').map((p) => p.file)).toEqual([
      'other/p01_dims.py',
      'pipeline/p01_dims.py',
    ])
  })

  it('shows each file as its header in the table does: a failed last attempt first, then the most urgent state', () => {
    const rows = buildRows(nodes, 0)
    const byFile = Object.fromEntries(summaries(nodes).map((p) => [p.file, p.shown?.label]))
    expect(byFile['pipeline/p01_dims.py']).toBe('stale')
    expect(byFile['pipeline/p02_reconcile.py']).toBe('failing')
    expect(byFile['other/p01_dims.py']).toBe('fresh')
    for (const group of groupByFile(rows)) {
      expect(summaries(nodes).find((p) => p.file === group.file)?.shown).toEqual(fileDisplay(group.rows))
    }
  })

  it('counts only the nodes of the kind, and keeps a file with none of it at 0', () => {
    const mixed = [...nodes, { ...node('other/p01_dims.py:ping'), kind: 'task' as const }]
    expect(summaries(mixed, mixed).find((p) => p.file === 'other/p01_dims.py')?.total).toBe(1)
    const tasks = summaries(mixed, mixed, 'task')
    expect(tasks.map((p) => [p.file, p.total])).toEqual([
      ['other/p01_dims.py', 1],
      ['pipeline/p01_dims.py', 0],
      ['pipeline/p02_reconcile.py', 0],
    ])
    expect(tasks.find((p) => p.total === 0)?.shown).toBeNull()
  })

  it('counts metadata before status arrives and keeps other pipelines when only one cone is loaded', () => {
    const pending = summaries([])
    expect(pending.map((p) => p.total)).toEqual([1, 2, 2])
    expect(pending.every((p) => p.shown === null)).toBe(true)
    const partial = summaries(nodes.filter((n) => n.id.startsWith('pipeline/p02')))
    expect(partial.map((p) => p.total)).toEqual([1, 2, 2])
    expect(partial.find((p) => p.file === 'pipeline/p02_reconcile.py')?.shown?.label).toBe('failing')
    expect(partial.find((p) => p.file === 'pipeline/p01_dims.py')?.shown).toBeNull()
  })

  it('knows a file only when every node of it has a state: a part says nothing of the whole', () => {
    expect(summaries(nodes).every((p) => p.shown !== null)).toBe(true)
    const part = nodes.filter((n) => n.id !== 'pipeline/p01_dims.py:dim_item')
    expect(summaries(part).find((p) => p.file === 'pipeline/p01_dims.py')?.shown).toBeNull()
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
