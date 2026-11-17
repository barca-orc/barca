import { describe, expect, it } from 'vitest'
import { HEALTH_ORDER, healthDisplay, healthWeight, worstHealth } from './healthDisplay'
import { ROOT, arcPath, buildHealthTree, healthSummary, leafIds, ringRadii, sunburst, treePath } from './healthTree'
import type { NodeGroup, NodeState } from './types'
import { served } from './testdata/served'

function node(id: string, inputs: string[], over: Partial<NodeState> = {}): NodeState {
  return served({
    id, name: id.split(':')[1]!, kind: 'asset', inputs, partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: { status: 'success', created_at: '2026-10-29 10:00:00', elapsed_seconds: 1, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null },
    shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, prev_run: null, late_since: null, signals: [], ...over,
  })
}
const failed = { last_materialization: { status: 'failed', created_at: '2026-10-29 11:00:00', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null, error: 'TypeError: x' } }
// What the server says of a node below a failure (`barca-core` walks the graph; these rows state it).
const blocked = { health: 'blocked' as const, health_reason: 'upstream_failing' as const }
const unknown = { cache: { state: 'unknown' as const, reason: 'cutoff_pending' as const, detail: '' } }

// a.py: raw (failing, 100 of 120 keys) → clean (unknown: a cutoff) → report (stale)
// b.py: src (fine) → mart (never run); c.py: x (stale on its own)
const states = [
  node('a.py:raw', [], { ...failed, partitioned: true, partitions: { total: 120, cached: 20, missing: 100, missing_keys: [], failed: 100 } }),
  node('a.py:clean', ['a.py:raw'], { ...unknown, ...blocked }),
  node('a.py:report', ['a.py:clean'], { cache: { state: 'stale', reason: 'upstream_stale', detail: '' }, ...blocked }),
  node('b.py:src', []),
  node('b.py:mart', ['b.py:src'], { cache: { state: 'never_run', reason: 'no_record', detail: '' }, last_materialization: null }),
  node('c.py:x', [], { cache: { state: 'stale', reason: 'changed', detail: '' } }),
  node('c.py:waits', [], unknown),
]
const graph = states.map((s) => ({ id: s.id, inputs: s.inputs }))
const group = (id: string, name: string, members: string[]): NodeGroup => ({ id, name, description: '', members, output: members.at(-1)! })

describe('health display', () => {
  it('has a word, a tone and a glyph for every health the server sends, most severe first', () => {
    expect(HEALTH_ORDER).toEqual(['failing', 'blocked', 'suspect', 'behind', 'undecided', 'never_run', 'ok'])
    expect(HEALTH_ORDER.map((h) => healthDisplay(h).label)).toEqual(['failing', 'blocked', 'suspect data', 'behind', 'undecided', 'never run', 'ok'])
    expect(worstHealth(['ok', 'behind', 'undecided'])).toBe('behind')
    expect(worstHealth([])).toBe('ok')
  })

  it('weighs a partitioned failure by its failed keys', () => {
    expect(healthWeight(states[0]!, 'failing')).toBe(100)
    expect(healthWeight(states[3]!, 'ok')).toBe(0)
    expect(healthWeight(states[5]!, 'behind')).toBe(1)
  })
})

describe('health tree', () => {
  it('shows a project without groups by file, every level the sum of its children', () => {
    const tree = buildHealthTree({ states, graph, groups: [] })
    const root = tree.nodes.get(ROOT)!
    expect(root.leaves).toBe(7)
    expect(root.counts).toMatchObject({ failing: 1, blocked: 2, behind: 1, undecided: 1, never_run: 1, ok: 1 })
    // a.py first: failing, and the heaviest (100 failed keys + 2 blocked).
    expect(root.children).toEqual(['file:a.py', 'file:c.py', 'file:b.py'])
    expect(tree.nodes.get('file:a.py')).toMatchObject({ health: 'failing', weight: 102, leaves: 3 })
    expect(tree.nodes.get('file:b.py')!.weight).toBe(0)
    expect(tree.blockedBy.get('a.py:report')).toBe('a.py:raw')
    expect(healthSummary(root)).toBe('1 failing · 2 blocked · 1 behind')
  })

  it('follows nested groups, with nodes in no group at the top', () => {
    const groups = [group('g:a', 'A', ['g:a1', 'a.py:report']), group('g:a1', 'A1', ['a.py:raw', 'a.py:clean'])]
    const tree = buildHealthTree({ states, graph, groups })
    expect(tree.nodes.get(ROOT)!.children[0]).toBe('g:a')
    expect(treePath(tree, 'a.py:clean').map((n) => n.id)).toEqual([ROOT, 'g:a', 'g:a1', 'a.py:clean'])
    expect(leafIds(tree, 'g:a').sort()).toEqual(['a.py:clean', 'a.py:raw', 'a.py:report'])
    expect(tree.nodes.get('g:a')).toMatchObject({ leaves: 3, health: 'failing' })
    expect(tree.nodes.get(ROOT)!.leaves).toBe(7)
  })

  it('sizes the map by how many nodes are underneath, worst first, children inside their parent', () => {
    const tree = buildHealthTree({ states, graph, groups: [] })
    const arcs = sunburst(tree, ROOT)
    const ring1 = arcs.filter((a) => a.depth === 1)
    expect(ring1.map((a) => a.id)).toEqual(['file:a.py', 'file:c.py', 'file:b.py'])
    expect(ring1[0]!.start).toBe(0)
    expect(ring1.at(-1)!.end).toBeCloseTo(1)
    const span = (a: { start: number; end: number }) => a.end - a.start
    // a.py holds 3 of the 7 nodes, b.py and c.py 2 each: 100 failed keys weigh no more than one node.
    expect(span(ring1[0]!)).toBeCloseTo(3 / 7)
    expect(span(ring1[1]!)).toBeCloseTo(2 / 7)
    for (const child of arcs.filter((a) => a.depth === 2)) {
      const parent = ring1.find((p) => tree.parent.get(child.id) === p.id)!
      expect(child.start).toBeGreaterThanOrEqual(parent.start - 1e-9)
      expect(child.end).toBeLessThanOrEqual(parent.end + 1e-9)
    }
    expect(sunburst(tree, 'b.py:src')).toEqual([])
  })

  it('sizes by assets: a task colours its group but takes no room, unless there is no asset at all', () => {
    const task = node('a.py:job', [], { ...failed, kind: 'task' })
    const tree = buildHealthTree({ states: [...states, task], graph, groups: [] })
    expect(tree.nodes.get('file:a.py')).toMatchObject({ leaves: 4, assets: 3 })
    const arcs = sunburst(tree, ROOT)
    expect(arcs.find((a) => a.id === 'file:a.py')!.size).toBe(3)
    expect(arcs.some((a) => a.id === 'a.py:job')).toBe(false)
    const tasks = buildHealthTree({ states: [task], graph: [], groups: [] })
    expect(sunburst(tasks, ROOT).map((a) => [a.id, a.size])).toEqual([['file:a.py', 1], ['a.py:job', 1]])
  })

  it('draws a partitioned node\'s keys outside it: failed, not up to date, ok', () => {
    const tree = buildHealthTree({ states, graph, groups: [] })
    expect(tree.nodes.get('a.py:raw')!.keys).toEqual({ failed: 100, behind: 0, ok: 20 })
    const keys = sunburst(tree, ROOT, 3).filter((a) => a.kind === 'keys')
    expect(keys.map((a) => [a.id, a.depth, a.health, a.label])).toEqual([
      ['a.py:raw::keys:failed', 3, 'failing', '100 keys failed'],
      ['a.py:raw::keys:ok', 3, 'ok', '20 keys ok'],
    ])
    const raw = sunburst(tree, ROOT, 3).find((a) => a.id === 'a.py:raw')!
    expect(keys[0]!.start).toBeCloseTo(raw.start)
    expect(keys[1]!.end).toBeCloseTo(raw.end)
    expect(keys[0]!.keys).toEqual({ count: 100, of: 120 })
    // A few failed keys among many stay visible, and the bands still fill the node.
    const big = buildHealthTree({ states: [node('k.py:k', [], { ...failed, partitioned: true, partitions: { total: 5000, cached: 4976, missing: 24, missing_keys: [], failed: 24 } })], graph: [], groups: [] })
    const bands = sunburst(big, 'k.py:k', 1, 0.02)
    expect(bands[0]!.end - bands[0]!.start).toBeCloseTo(0.01)
    expect(bands.at(-1)!.end).toBeCloseTo(1)
    // Not past the last ring, and not under a node too thin to hold them.
    expect(sunburst(tree, ROOT, 2).some((a) => a.kind === 'keys')).toBe(false)
    expect(sunburst(tree, ROOT, 3, 0.5).some((a) => a.kind === 'keys')).toBe(false)
    // A node at the centre shows its keys as the first ring.
    expect(sunburst(tree, 'a.py:raw').map((a) => a.id)).toEqual(['a.py:raw::keys:failed', 'a.py:raw::keys:ok'])
  })

  it('draws members too thin to show one by one as one band per colour, so a ring never frays', () => {
    const ok = Array.from({ length: 60 }, (_, i) => node(`m.py:n${String(i).padStart(2, '0')}`, []))
    const late = Array.from({ length: 30 }, (_, i) => node(`m.py:s${String(i).padStart(2, '0')}`, [], { cache: { state: 'stale', reason: 'changed', detail: '' } }))
    const broken = node('m.py:broken', [], { ...failed })
    const t = buildHealthTree({ states: [broken, ...ok, ...late], graph: [], groups: [] })
    const arcs = sunburst(t, 'file:m.py', 1, 0.02)
    expect(arcs.map((a) => [a.id, a.kind, a.label, a.leaves, a.health])).toEqual([
      ['file:m.py::rest:failing', 'rest', '91 smaller', 1, 'failing'],
      ['file:m.py::rest:attention', 'rest', '91 smaller', 30, 'behind'],
      ['file:m.py::rest:ok', 'rest', '91 smaller', 60, 'ok'],
    ])
    // The ring is whole: the segments tile the turn.
    expect(arcs.reduce((sum, a) => sum + (a.end - a.start), 0)).toBeCloseTo(1)
  })
  it('sizes the thin members\' bands by the nodes inside them, so mostly-fine small groups read mostly ok', () => {
    // 20 groups of 5: each has one stale node, so each group alone is `behind`.
    const members = Array.from({ length: 20 }, (_, g) => Array.from({ length: 5 }, (_, i) => node(`g.py:g${g}n${i}`, [], i === 0 ? { cache: { state: 'stale', reason: 'changed', detail: '' } } : {})))
    const groups = members.map((m, g) => group(`g:${g}`, `G${g}`, m.map((n) => n.id)))
    const t = buildHealthTree({ states: members.flat(), graph: [], groups: [group('g:all', 'All', groups.map((g) => g.id)), ...groups] })
    const bands = sunburst(t, 'g:all', 1, 0.1)
    expect(bands.map((a) => [a.id, a.health, a.leaves, a.label])).toEqual([
      ['g:all::rest:attention', 'behind', 20, '20 smaller'],
      ['g:all::rest:ok', 'ok', 80, '20 smaller'],
    ])
    expect(bands[1]!.end - bands[1]!.start).toBeCloseTo(0.8)
  })
})

describe('arc geometry', () => {
  const angles = (d: string) => [...d.matchAll(/(-?\d+\.\d+) (-?\d+\.\d+)/g)].map((m) => Math.atan2(Number(m[1]), -Number(m[2])))
  it('parts neighbours by an even gap at every radius', () => {
    const plain = angles(arcPath(0, 0.25, 50, 100))
    const gapped = angles(arcPath(0, 0.25, 50, 100, 2))
    // The outer edge moves in by 1 px at radius 100, the inner by 1 px at radius 50: twice the angle.
    expect(gapped[0]! - plain[0]!).toBeCloseTo(1 / 100)
    expect(gapped[3]! - plain[3]!).toBeCloseTo(1 / 50)
    // A sector thinner than its gaps keeps a hairline instead of turning inside out.
    expect(() => arcPath(0.1, 0.1001, 50, 100, 4)).not.toThrow()
    expect(arcPath(0, 1, 50, 100, 2)).toContain('A 100 100 0 1 1')
  })
})

describe('ring radii', () => {
  it('thins each ring out, fills the space, and never makes one level a thick band', () => {
    const shape = { firstMax: 0.27, decay: 0.8, gap: 1 }
    const rings = ringRadii(4, 30, 150, shape)
    expect(rings).toHaveLength(4)
    const widths = rings.map(([a, b]) => b - a)
    for (let k = 1; k < 4; k++) expect(widths[k]! / widths[k - 1]!).toBeCloseTo(0.8)
    expect(rings[0]![0]).toBe(31)
    expect(rings[3]![1]).toBeCloseTo(150)
    // One level: as wide as the cap allows, not the whole span.
    const one = ringRadii(1, 30, 150, shape)
    expect(one[0]![1] - one[0]![0]).toBeCloseTo(150 * 0.27)
    expect(ringRadii(0, 30, 150, shape)).toEqual([])
  })
})
