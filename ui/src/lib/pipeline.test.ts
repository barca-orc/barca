import { describe, expect, it } from 'vitest'
import type { CacheStatus, NodeState } from './types'
import { inPipeline, pipelineSummaries, sourceFile } from './pipeline'

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

  it('one entry per source file, in file order, with its node count', () => {
    const s = pipelineSummaries(nodes)
    expect(s.map((p) => [p.file, p.total])).toEqual([
      ['other/p01_dims.py', 1],
      ['pipeline/p01_dims.py', 2],
      ['pipeline/p02_reconcile.py', 2],
    ])
  })

  it('names are the file name; files are what tell same-named pipelines apart', () => {
    const s = pipelineSummaries(nodes)
    expect(s.filter((p) => p.name === 'p01_dims.py').map((p) => p.file)).toEqual([
      'other/p01_dims.py',
      'pipeline/p01_dims.py',
    ])
  })

  it('reports the most urgent state in each pipeline', () => {
    const byFile = Object.fromEntries(pipelineSummaries(nodes).map((p) => [p.file, p.worst]))
    expect(byFile['pipeline/p01_dims.py']).toBe('stale')
    expect(byFile['pipeline/p02_reconcile.py']).toBe('failed')
    expect(byFile['other/p01_dims.py']).toBe('cached')
  })

  it('counts metadata before status arrives and keeps other pipelines when only one cone is loaded', () => {
    const assets = nodes.map(n => ({ id: n.id }))
    const pending = pipelineSummaries([], assets)
    expect(pending.map(p => p.total)).toEqual([1, 2, 2])
    expect(pending.every(p => p.worst === 'unknown')).toBe(true)
    const partial = pipelineSummaries(nodes.filter(n => n.id.startsWith('pipeline/p02')), assets)
    expect(partial.map(p => p.total)).toEqual([1, 2, 2])
    expect(partial.find(p => p.file === 'pipeline/p02_reconcile.py')?.worst).toBe('failed')
    expect(partial.find(p => p.file === 'pipeline/p01_dims.py')?.worst).toBe('unknown')
  })

  it('knows a file only when every node of it has a state: a part says nothing of the whole', () => {
    const assets = nodes.map(n => ({ id: n.id }))
    expect(pipelineSummaries(nodes, assets).every(p => p.known)).toBe(true)
    const part = nodes.filter(n => n.id !== 'pipeline/p01_dims.py:dim_item')
    expect(pipelineSummaries(part, assets).find(p => p.file === 'pipeline/p01_dims.py')?.known).toBe(false)
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
