import { describe, it, expect } from 'vitest'
import { buildGraph, edgeClassName, graphAt, impliedEdges, layoutInput, shortName } from './graph'
import { layout, routeMiddle, routePath } from './graphLayout'
import type { ListNode } from './types'
import type { Tone } from './tone'

const ASSETS: ListNode[] = [
  { id: 'p.py:a', kind: 'asset', freshness: 'always', env: [], file: 'p.py', line: 1, inputs: [] },
  { id: 'p.py:b', kind: 'asset', freshness: 'always', env: [], file: 'p.py', line: 1, inputs: ['p.py:a'] },
  { id: 'p.py:c', kind: 'task', freshness: 'manual', env: [], file: 'p.py', line: 1, inputs: ['p.py:b'] },
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

  it('carries kind + short name, and rests neutral (state is overlaid later)', () => {
    const { nodes } = buildGraph(ASSETS, 'LR')
    expect(nodes[0]!.data.name).toBe('a')
    expect(nodes[2]!.data.kind).toBe('task')
    const resting: Tone = 'neutral'
    expect(nodes.every((n) => n.data.tone === resting)).toBe(true)
  })

  it('drops edges whose input is not in the asset set', () => {
    const orphan: ListNode[] = [
      { id: 'p.py:x', kind: 'asset', freshness: 'always', env: [], file: 'p.py', line: 1, inputs: ['p.py:missing'] },
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

describe('edgeClassName', () => {
  it('marks an edge live when a fresh upstream feeds a running downstream', () => {
    expect(edgeClassName({ sourceTone: 'success', targetTone: 'running', hot: false })).toBe('live')
  })

  it('marks an edge hot when it touches the selection', () => {
    expect(edgeClassName({ hot: true })).toBe('hot')
  })

  it('can be both live and hot', () => {
    expect(edgeClassName({ sourceTone: 'success', targetTone: 'running', hot: true })).toBe('live hot')
  })

  it('is empty for an inert edge', () => {
    expect(edgeClassName({ sourceTone: 'queued', targetTone: 'queued', hot: false })).toBe('')
  })
})

describe('layout', () => {
  const box = (id: string) => ({ id, width: 100, height: 50 })
  it('packs unconnected nodes in rows, not one column', () => {
    const nodes = Array.from({ length: 16 }, (_, i) => box(`n${i}`))
    const out = layout({ dir: 'LR', ranksep: 96, nodes, edges: [] }).positions
    expect(new Set(Object.values(out).map((p) => p.y)).size).toBeLessThan(6)
    expect(new Set(Object.values(out).map((p) => `${p.x},${p.y}`)).size).toBe(16)
  })

  it('keeps a connected part together, and puts first nodes ahead of it', () => {
    const out = layout({
      dir: 'LR',
      ranksep: 96,
      nodes: [box('a'), box('b'), { ...box('port'), first: true }],
      edges: [['a', 'b']],
    }).positions
    expect(out.a!.y).toBe(out.b!.y)
    expect(out.b!.x - out.a!.x).toBe(196)
    expect(out.port!.y).toBeLessThanOrEqual(out.a!.y)
  })
})

describe('edge routes', () => {
  // A diamond with a long edge: a → b → c → d and a → d, so a → d passes two ranks of boxes.
  const box = (id: string) => ({ id, width: 208, height: 40 })
  const input = { dir: 'LR' as const, ranksep: 80, nodes: ['a', 'b', 'c', 'd', 'e', 'f'].map(box), edges: [['a', 'b'], ['b', 'c'], ['c', 'd'], ['a', 'd'], ['a', 'e'], ['e', 'f'], ['f', 'd']] as [string, string][] }
  const { positions, routes } = layout(input)
  const rects = Object.entries(positions).map(([id, p]) => ({ id, x0: p.x - 104, x1: p.x + 104, y0: p.y - 20, y1: p.y + 20 }))
  /** Points along a route's path (straight runs and S-curves sampled). */
  const sample = (route: { x: number; y: number }[]) => {
    const out: { x: number; y: number }[] = []
    for (let i = 1; i < route.length; i++) {
      const a = route[i - 1]!, b = route[i]!, mid = (a.x + b.x) / 2
      for (let t = 0; t <= 1; t += 0.05) {
        const u = 1 - t
        // The cubic routePath draws: controls at the midpoint along the ranks.
        const x = a.y === b.y ? a.x + (b.x - a.x) * t : u * u * u * a.x + 3 * u * u * t * mid + 3 * u * t * t * mid + t * t * t * b.x
        const y = a.y === b.y ? a.y : u * u * u * a.y + 3 * u * u * t * a.y + 3 * u * t * t * b.y + t * t * t * b.y
        out.push({ x, y })
      }
    }
    return out
  }

  it('route every edge from the source\'s side to the target\'s', () => {
    for (const [source, target] of input.edges) {
      const route = routes[`${source}->${target}`]!
      expect(route[0]).toEqual({ x: positions[source]!.x + 104, y: positions[source]!.y })
      expect(route.at(-1)).toEqual({ x: positions[target]!.x - 104, y: positions[target]!.y })
    }
  })

  it('never cross a card', () => {
    // The long edge passes the ranks of b and c: it runs between their boxes.
    expect(routes['a->d']!.length).toBeGreaterThan(4)
    for (const [source, target] of input.edges) {
      for (const p of sample(routes[`${source}->${target}`]!)) {
        const inside = rects.filter((r) => p.x > r.x0 + 0.5 && p.x < r.x1 - 0.5 && p.y > r.y0 + 0.5 && p.y < r.y1 - 0.5)
        expect(inside.map((r) => r.id), `${source}->${target} at ${p.x},${p.y}`).toEqual([])
      }
    }
  })

  it('are drawn straight through ranks and curved between them', () => {
    expect(routePath([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 30, y: 20 }], 'LR')).toBe('M0,0 L10,0 C20,0 20,20 30,20')
    expect(routePath([{ x: 0, y: 0 }, { x: 20, y: 30 }], 'TB')).toBe('M0,0 C0,15 20,15 20,30')
    expect(routeMiddle([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 30, y: 20 }])).toEqual({ x: 20, y: 10 })
  })

  it('reach React Flow on each edge, with its direction', () => {
    const { edges } = buildGraph(ASSETS, 'TB')
    expect(edges[0]!.type).toBe('routed')
    expect(edges[0]!.data).toMatchObject({ direction: 'TB', route: expect.any(Array) })
  })
})

describe('impliedEdges', () => {
  const group = (id: string, inputs: string[] = []) => ({ id, kind: 'group' as const, inputs })
  it('finds the edges of a graph of groups that another path already shows, and leaves them out of the layout', () => {
    const entries = [group('a'), group('b', ['a']), group('c', ['a', 'b']), group('d', ['a', 'c'])]
    expect([...impliedEdges(entries)].sort()).toEqual(['a->c', 'a->d'])
    expect(layoutInput(entries, 'LR').edges).toEqual([['a', 'b'], ['b', 'c'], ['c', 'd']])
    // Still in the graph, drawn for the selection.
    expect(buildGraph(entries, 'LR').edges.filter((e) => e.data?.implied).map((e) => e.id)).toEqual(['a->c', 'a->d'])
  })
  it('keeps every edge of a graph of nodes: each is a real input', () => {
    const nodes = [{ id: 'a', kind: 'asset' as const, inputs: [] }, { id: 'b', kind: 'asset' as const, inputs: ['a'] }, { id: 'c', kind: 'asset' as const, inputs: ['a', 'b'] }]
    expect(impliedEdges(nodes).size).toBe(0)
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
      { weight: undefined, port: true, loose: true, direction: 'LR' },
      { weight: 8, port: true, loose: true, direction: 'LR' },
    ])
  })
})
