import { describe, it, expect } from 'vitest'
import { buildGraph, edgeClassName, overlayRunStatus, shortName } from './graph'
import type { AssetSummary, StatusKind } from './types'

const ASSETS: AssetSummary[] = [
  { id: 'p.py:a', kind: 'asset', freshness: { type: 'Always' }, env: [], inputs: [] },
  { id: 'p.py:b', kind: 'asset', freshness: { type: 'Always' }, env: [], inputs: ['p.py:a'] },
  { id: 'p.py:c', kind: 'task', freshness: { type: 'Manual' }, env: [], inputs: ['p.py:b'] },
]

describe('shortName', () => {
  it('takes the part after the last colon', () => {
    expect(shortName('iris_project/assets.py:raw_data')).toBe('raw_data')
    expect(shortName('bare')).toBe('bare')
  })
})

describe('buildGraph', () => {
  it('produces one node per asset and one edge per resolved input', () => {
    const { nodes, edges } = buildGraph(ASSETS, 'LR')
    expect(nodes.map((n) => n.id)).toEqual(['p.py:a', 'p.py:b', 'p.py:c'])
    expect(edges.map((e) => e.id)).toEqual(['p.py:a->p.py:b', 'p.py:b->p.py:c'])
  })

  it('carries kind + short name, and rests at queued (status is overlaid later)', () => {
    const { nodes } = buildGraph(ASSETS, 'LR')
    expect(nodes[0]!.data.name).toBe('a')
    expect(nodes[2]!.data.kind).toBe('task')
    expect(nodes.every((n) => n.data.status === 'queued')).toBe(true)
  })

  it('drops edges whose input is not in the asset set', () => {
    const orphan: AssetSummary[] = [
      { id: 'p.py:x', kind: 'asset', freshness: { type: 'Always' }, env: [], inputs: ['p.py:missing'] },
    ]
    expect(buildGraph(orphan, 'LR').edges).toEqual([])
  })
})

describe('overlayRunStatus', () => {
  it('adds optimistic running for the triggered node', () => {
    expect(overlayRunStatus({}, true, 'n')).toEqual({ n: 'running' })
  })

  it('does not override a status the stream already set', () => {
    const base: Record<string, StatusKind> = { n: 'success' }
    expect(overlayRunStatus(base, true, 'n')).toBe(base)
  })

  it('is a no-op when not running or no node', () => {
    expect(overlayRunStatus({}, false, 'n')).toEqual({})
    expect(overlayRunStatus({}, true, null)).toEqual({})
  })
})

describe('edgeClassName', () => {
  it('marks an edge live when a fresh upstream feeds a running downstream', () => {
    expect(edgeClassName({ sourceStatus: 'success', targetStatus: 'running', hot: false })).toBe('live')
  })

  it('marks an edge hot when it touches the selection', () => {
    expect(edgeClassName({ hot: true })).toBe('hot')
  })

  it('can be both live and hot', () => {
    expect(edgeClassName({ sourceStatus: 'success', targetStatus: 'running', hot: true })).toBe('live hot')
  })

  it('is empty for an inert edge', () => {
    expect(edgeClassName({ sourceStatus: 'queued', targetStatus: 'queued', hot: false })).toBe('')
  })
})
