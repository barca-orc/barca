import { describe, expect, it } from 'vitest'
import { blockedCauses, healthWeight, nodeHealth, worstHealth } from './nodeHealth'
import { ROOT, buildHealthTree, healthSummary, leafIds, sunburst, treePath } from './healthTree'
import type { NodeGroup, NodeState } from './types'

function node(id: string, inputs: string[], over: Partial<NodeState> = {}): NodeState {
  return {
    id, name: id.split(':')[1]!, kind: 'asset', inputs, partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: { status: 'success', created_at: '2026-10-29 10:00:00', elapsed_seconds: 1, run_hash: null, artifact: null, format: null, size_bytes: null },
    shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, ...over,
  }
}
const failed = { last_materialization: { status: 'failed', created_at: '2026-10-29 11:00:00', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, error: 'TypeError: x' } }
const unknown = { cache: { state: 'unknown' as const, reason: 'cutoff_pending' as const, detail: '' } }

// a.py: raw (failing, 100 of 120 keys) → clean (unknown: a cutoff) → report (stale)
// b.py: src (fine) → mart (never run); c.py: x (stale on its own)
const states = [
  node('a.py:raw', [], { ...failed, partitioned: true, partitions: { total: 120, cached: 20, missing: 100, missing_keys: [], failed: 100 } }),
  node('a.py:clean', ['a.py:raw'], unknown),
  node('a.py:report', ['a.py:clean'], { cache: { state: 'stale', reason: 'upstream_stale', detail: '' } }),
  node('b.py:src', []),
  node('b.py:mart', ['b.py:src'], { cache: { state: 'never_run', reason: 'no_record', detail: '' }, last_materialization: null }),
  node('c.py:x', [], { cache: { state: 'stale', reason: 'changed', detail: '' } }),
  node('c.py:waits', [], unknown),
]
const graph = states.map((s) => ({ id: s.id, inputs: s.inputs }))
const group = (id: string, name: string, members: string[]): NodeGroup => ({ id, name, description: '', members, output: members.at(-1)! })

describe('node health', () => {
  it('names one word per node, most severe fact first', () => {
    expect(nodeHealth(states[0]!)).toBe('failing')
    expect(nodeHealth(states[1]!)).toBe('waiting')
    expect(nodeHealth(states[1]!, { blocked: true })).toBe('blocked')
    expect(nodeHealth(states[3]!, { suspect: true })).toBe('suspect')
    expect(nodeHealth(states[3]!, { late: true })).toBe('behind')
    expect(nodeHealth(states[4]!)).toBe('never_run')
    expect(nodeHealth(states[5]!)).toBe('behind')
    expect(nodeHealth(node('t.py:t', [], { kind: 'task', cache: { state: 'always_runs', reason: 'task', detail: '' } }))).toBe('ok')
    expect(nodeHealth(node('t.py:t', [], { kind: 'task', cache: { state: 'always_runs', reason: 'task', detail: '' }, last_materialization: null }))).toBe('never_run')
    expect(worstHealth(['ok', 'behind', 'waiting'])).toBe('behind')
    expect(worstHealth([])).toBe('ok')
  })

  it('weighs a partitioned failure by its failed keys', () => {
    expect(healthWeight(states[0]!, 'failing')).toBe(100)
    expect(healthWeight(states[3]!, 'ok')).toBe(0)
    expect(healthWeight(states[5]!, 'behind')).toBe(1)
  })

  it('passes a failure down through unknown and stale nodes, but not through fresh or pinned ones', () => {
    const byId = new Map(states.map((s) => [s.id, s]))
    const inputs = new Map(states.map((s) => [s.id, s.inputs]))
    expect(blockedCauses(byId, inputs)).toEqual(new Map([['a.py:clean', 'a.py:raw'], ['a.py:report', 'a.py:raw']]))
    expect(blockedCauses(byId, inputs, new Set(['a.py:clean']))).toEqual(new Map())
  })
})

describe('health tree', () => {
  it('shows a project without groups by file, every level the sum of its children', () => {
    const tree = buildHealthTree({ states, graph, groups: [] })
    const root = tree.nodes.get(ROOT)!
    expect(root.leaves).toBe(7)
    expect(root.counts).toMatchObject({ failing: 1, blocked: 2, behind: 1, waiting: 1, never_run: 1, ok: 1 })
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

  it('sizes the map by what is wrong, keeping healthy scopes as thin slices', () => {
    const tree = buildHealthTree({ states, graph, groups: [] })
    const arcs = sunburst(tree, ROOT)
    const ring1 = arcs.filter((a) => a.depth === 1)
    expect(ring1.map((a) => a.id)).toEqual(['file:a.py', 'file:c.py', 'file:b.py'])
    expect(ring1[0]!.start).toBe(0)
    expect(ring1.at(-1)!.end).toBeCloseTo(1)
    const span = (a: { start: number; end: number }) => a.end - a.start
    expect(span(ring1[0]!)).toBeGreaterThan(0.8)
    expect(span(ring1[2]!)).toBeGreaterThan(0)
    // Children sit inside their parent's span.
    for (const child of arcs.filter((a) => a.depth === 2)) {
      const parent = ring1.find((p) => tree.parent.get(child.id) === p.id)!
      expect(child.start).toBeGreaterThanOrEqual(parent.start - 1e-9)
      expect(child.end).toBeLessThanOrEqual(parent.end + 1e-9)
    }
    expect(sunburst(tree, 'a.py:raw')).toEqual([])
  })
})
