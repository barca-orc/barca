import { describe, it, expect } from 'vitest'
import { consumersOf, downstreamIds, failedUpstream, failures, headline, healthCounts, healthLabel, healthParts, scopeAge } from './health'
import type { NodeState } from './types'

function node(id: string, inputs: string[], over: Partial<NodeState> = {}): NodeState {
  return {
    id, name: id.split(':')[1]!, kind: 'asset', inputs, partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: { status: 'success', created_at: '2026-10-29 10:00:00', elapsed_seconds: 1, run_hash: null, artifact: null, format: null, size_bytes: null },
    shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, ...over,
  }
}
const failedAt = (at: string, error: string) => ({
  last_materialization: { status: 'failed', created_at: at, elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, error },
})

// raw → clean → (report, check); orphan fails alone.
const nodes = [
  node('p.py:raw', [], failedAt('2026-10-29 11:00:00', "KeyError: 'currency'\n  File \"p.py\", line 3")),
  node('p.py:clean', ['p.py:raw'], { cache: { state: 'stale', reason: 'upstream_stale', detail: '' } }),
  node('p.py:report', ['p.py:clean']),
  node('p.py:check', ['p.py:clean'], { kind: 'task' }),
  node('p.py:orphan', [], failedAt('2026-10-29 12:00:00', 'ZeroDivisionError: division by zero')),
]
const byId = new Map(nodes.map((n) => [n.id, n]))
const consumers = consumersOf(nodes)

describe('health', () => {
  it('counts a scope by bucket, a live run first', () => {
    const ids = nodes.map((n) => n.id)
    expect(healthCounts(ids, byId)).toEqual({ failed: 2, running: 0, stale: 1, idle: 0, healthy: 2, pending: 0, total: 5 })
    expect(healthCounts([...ids, 'p.py:unloaded'], byId, { 'p.py:report': 'running' })).toMatchObject({ running: 1, healthy: 1, pending: 1 })
    expect(healthLabel(healthCounts(ids, byId))).toBe('2 failed · 1 stale · 2 healthy')
    expect(healthParts(healthCounts(ids, byId), 2)).toEqual([{ bucket: 'failed', text: '2 failed' }, { bucket: 'stale', text: '1 stale' }])
    expect(healthLabel(healthCounts(ids, byId), 1)).toBe('2 failed')
  })

  it('lists failures by downstream impact, with their headline', () => {
    const list = failures(nodes, consumers)
    expect(list.map((f) => [f.name, f.impact, f.headline])).toEqual([
      ['raw', 3, "KeyError: 'currency'"],
      ['orphan', 0, 'ZeroDivisionError: division by zero'],
    ])
  })

  it('finds what blocks a node, nearest first', () => {
    const inputs = new Map(nodes.map((n) => [n.id, n.inputs]))
    expect(failedUpstream('p.py:report', inputs, byId)).toEqual(['p.py:raw'])
    expect(failedUpstream('p.py:orphan', inputs, byId)).toEqual([])
    expect([...downstreamIds('p.py:raw', consumers)].sort()).toEqual(['p.py:check', 'p.py:clean', 'p.py:report'])
  })

  it('says something for an empty error', () => {
    expect(headline('\n\n  ValueError: x\n trace')).toBe('ValueError: x')
    expect(headline(null)).toBe('failed, with no error recorded')
  })
})

describe('scopeAge', () => {
  const aged = (id: string, ok: string | null, since: string | null, runs: number) =>
    ({ id, last_success_at: ok, failing_since: since, consecutive_failures: runs }) as unknown as NodeState

  it('reports the worst of its members', () => {
    const byId = new Map([
      ['a', aged('a', '2026-10-28 10:00:00', '2026-10-29 01:00:00', 3)],
      ['b', aged('b', '2026-10-29 10:00:00', '2026-10-29 05:00:00', 1)],
      ['c', aged('c', null, null, 0)],
    ])
    expect(scopeAge(['a', 'b', 'c', 'unloaded'], byId)).toEqual({
      lastSuccessAt: '2026-10-28 10:00:00', failingSince: '2026-10-29 01:00:00', failures: 3,
    })
    expect(scopeAge(['c'], byId)).toEqual({ lastSuccessAt: null, failingSince: null, failures: 0 })
  })
})
