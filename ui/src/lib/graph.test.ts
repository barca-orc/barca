import { describe, it, expect } from 'vitest'
import { buildGraph, edgeClassName, graphAt, layoutInput, overlayRunStatus, shortName } from './graph'
import { layout } from './graphLayout'
import type { ListNode, StatusKind } from './types'

const ASSETS: ListNode[] = [
  { id: 'p.py:a', kind: 'asset', freshness: 'always', env: [], inputs: [] },
  { id: 'p.py:b', kind: 'asset', freshness: 'always', env: [], inputs: ['p.py:a'] },
  { id: 'p.py:c', kind: 'task', freshness: 'manual', env: [], inputs: ['p.py:b'] },
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

  it('carries kind + short name, and rests neutral (status is overlaid later)', () => {
    const { nodes } = buildGraph(ASSETS, 'LR')
    expect(nodes[0]!.data.name).toBe('a')
    expect(nodes[2]!.data.kind).toBe('task')
    expect(nodes.every((n) => n.data.status === 'skipped')).toBe(true)
  })

  it('drops edges whose input is not in the asset set', () => {
    const orphan: ListNode[] = [
      { id: 'p.py:x', kind: 'asset', freshness: 'always', env: [], inputs: ['p.py:missing'] },
    ]
    expect(buildGraph(orphan, 'LR').edges).toEqual([])
  })
})

describe('layoutInput / graphAt', () => {
  it('lay out in two steps (as the worker does) what buildGraph lays out in one', () => {
    const input = layoutInput(ASSETS, 'TB')
    // Plain data: what is posted to the worker.
    expect(JSON.parse(JSON.stringify(input))).toEqual(input)
    expect(graphAt(ASSETS, 'TB', layout(input))).toEqual(buildGraph(ASSETS, 'TB'))
  })

  it('gives dagre only the dependencies inside the asset set', () => {
    const orphan: ListNode[] = [{ ...ASSETS[1]!, inputs: ['p.py:missing'] }]
    expect(layoutInput(orphan, 'LR').edges).toEqual([])
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

describe('layout', () => {
  const box = (id: string) => ({ id, width: 100, height: 50 })
  it('packs unconnected nodes in rows, not one column', () => {
    const nodes = Array.from({ length: 16 }, (_, i) => box(`n${i}`))
    const out = layout({ dir: 'LR', ranksep: 96, nodes, edges: [] })
    expect(new Set(Object.values(out).map((p) => p.y)).size).toBeLessThan(6)
    expect(new Set(Object.values(out).map((p) => `${p.x},${p.y}`)).size).toBe(16)
  })

  it('keeps a connected part together, and puts first nodes ahead of it', () => {
    const out = layout({
      dir: 'LR',
      ranksep: 96,
      nodes: [box('a'), box('b'), { ...box('port'), first: true }],
      edges: [['a', 'b']],
    })
    expect(out.a!.y).toBe(out.b!.y)
    expect(out.b!.x - out.a!.x).toBe(196)
    expect(out.port!.y).toBeLessThanOrEqual(out.a!.y)
  })
})

describe('loose entries', () => {
  const entries = [
    { id: 'x', kind: 'port' as const, inputs: [], loose: true },
    { id: 'a', kind: 'asset' as const, inputs: ['x'] },
    { id: 'b', kind: 'asset' as const, inputs: ['x'], inputWeights: { x: 8 } },
  ]
  it('keep their edges out of the layout but not out of the graph', () => {
    expect(layoutInput(entries, 'LR').edges).toEqual([])
    const { edges } = buildGraph(entries, 'LR')
    expect(edges.map((e) => e.data)).toEqual([
      { weight: undefined, port: true, loose: true },
      { weight: 8, port: true, loose: true },
    ])
  })
})
