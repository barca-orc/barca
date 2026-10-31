import { describe, it, expect } from 'vitest'
import { causeOf, failedKeyNodes, failureGroups, impactLabel, keysLine, type Failure, consumersOf, downstreamIds, failedUpstream, failures, headline, healthCounts, healthLabel, healthParts, scopeAge } from './health'
import type { NodeState, PartitionKeys } from './types'

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

  it('does not call an unloaded graph zero', () => {
    expect(failures(nodes, null).every((f) => f.impact === null)).toBe(true)
    expect(impactLabel(null)).toBe('?')
    expect(impactLabel(0)).toBe('0')
  })

  it('groups failures by cause, numbers normalised, most impact first', () => {
    const f = (headline: string, impact: number | null) => ({ headline, impact }) as Failure
    const groups = failureGroups([f('fell 64%', 2), f('fell 1,200%', 3), f('other', 9), f('later', null)])
    expect(groups.map((g) => [g.cause, g.failures.length, g.impact])).toEqual([['other', 1, 9], ['fell #%', 2, 5], ['later', 1, null]])
    // The heading shows what its first member says, not the masked cause.
    expect(groups.map((g) => g.headline)).toEqual(['other', 'fell 64%', 'later'])
    expect(causeOf('v1.2 failed 3 times')).toBe('v# failed # times')
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

describe('headline', () => {
  it('skips the frames of a bare traceback', () => {
    expect(headline('Traceback (most recent call last):\n  File "/a.py", line 1\nKeyError: \'x\'\n')).toBe("KeyError: 'x'")
    expect(headline('  File "/a.py", line 1\nValueError: no')).toBe('ValueError: no')
  })
  it('keeps a first line that says what went wrong', () => {
    expect(headline('ValueError: no\nTraceback (most recent call last):\n  File "/a.py"')).toBe('ValueError: no')
  })
})

describe('partitioned failures', () => {
  const KEY_ERROR = 'BatchKeyFailed: forecast model diverged for <key>: NaN in week 14'
  const SAMPLE = 'BatchKeyFailed: forecast model diverged for ra-02541: NaN in week 14\n  File "p.py", line 9'
  const page: PartitionKeys = {
    total: 7, recorded: 3077, failed: 7,
    keys: [{ key: 'account=ra-02541', status: 'failed', created_at: '2026-10-31 15:30:12', elapsed_seconds: null, error: SAMPLE, run_id: 'd9becbdf4a92', artifact_path: null, size_bytes: null }],
    errors: [{ error: KEY_ERROR, keys: 5 }, { error: 'KeyError: <key>', keys: 2 }],
  }
  const forecast = (over: Partial<NodeState> = {}) =>
    node('p.py:forecast', [], {
      partitioned: true,
      cache: { state: 'stale', reason: 'failed', detail: '' },
      partitions: { total: 3077, cached: 3070, missing: 7, missing_keys: [], failed: 7, failed_keys: [] },
      ...over,
    })
  const loaded = new Map([['p.py:forecast', page]])

  it('counts the failed keys until their errors are loaded, then heads with the most common', () => {
    const [before] = failures([forecast()], consumers)
    expect(before).toMatchObject({ headline: '7 of 3,077 keys failed', error: '', keys: { failed: 7, total: 3077, errors: null, runId: null } })
    expect(keysLine(before!.keys!)).toBe('7 of 3,077 keys')
    const [after] = failures([forecast()], consumers, loaded)
    expect(after).toMatchObject({ headline: KEY_ERROR, error: SAMPLE, keys: { failed: 7, total: 3077, errors: page.errors, runId: 'd9becbdf4a92' } })
    expect(keysLine(after!.keys!)).toBe('7 of 3,077 keys · +1 more error')
  })

  it('a key error outranks the node-level one, which is one key\'s', () => {
    const withError = forecast(failedAt('2026-10-31 15:30:12', 'BatchKeyFailed: forecast model diverged for ra-02541: NaN in week 14'))
    expect(failures([withError], consumers)[0]!.headline).toBe('BatchKeyFailed: forecast model diverged for ra-02541: NaN in week 14')
    expect(failures([withError], consumers, loaded)[0]!.headline).toBe(KEY_ERROR)
    expect(failureGroups(failures([withError], consumers, loaded))[0]!.headline).toBe(KEY_ERROR)
  })

  it('leaves a node whose keys did not fail alone', () => {
    // Failed as a whole, with 7 keys stale rather than failed: its own error, and no key errors to load.
    const whole = forecast({ ...failedAt('2026-10-31 15:30:12', 'ImportError: no module named prophet'), partitions: { total: 3077, cached: 3070, missing: 7, missing_keys: [] } })
    expect(failures([whole], consumers)[0]).toMatchObject({ headline: 'ImportError: no module named prophet', keys: null, missingKeys: 7 })
    expect(failedKeyNodes([forecast(), whole, ...nodes])).toEqual(['p.py:forecast'])
  })
})
