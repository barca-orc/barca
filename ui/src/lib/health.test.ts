import { describe, it, expect } from 'vitest'
import { failedKeyNodes, keysLine, consumersOf, downstreamIds, failures, headline, streakLabel, relativeTraceback, pathBreaks } from './health'
import { blockerOf } from './healthDisplay'
import { failureRoot } from './testdata/rollup'
import type { NodeState, PartitionKeys } from './types'
import { ROLLUP, STATE, row } from './testdata/project'
import { blockedBy, served } from './testdata/served'

function node(id: string, inputs: string[], over: Partial<NodeState> = {}): NodeState {
  return served({
    id, name: id.split(':')[1]!, kind: 'asset', inputs, partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: { status: 'success', created_at: '2026-10-29 10:00:00', elapsed_seconds: 1, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null },
    shape: null, env: [], durations: null, next_run: null, prev_run: null, late_since: null, signals: [], last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, ...over,
  })
}
const failedAt = (at: string, error: string) => ({
  last_materialization: { status: 'failed', created_at: at, elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null, error },
})

// raw → clean → (report, check); orphan fails alone.
const nodes = [
  node('p.py:raw', [], failedAt('2026-10-29 11:00:00', "KeyError: 'currency'\n  File \"p.py\", line 3")),
  node('p.py:clean', ['p.py:raw'], { cache: { state: 'stale', reason: 'upstream_stale', detail: '' }, ...blockedBy('p.py:raw') }),
  node('p.py:report', ['p.py:clean']),
  node('p.py:check', ['p.py:clean'], { kind: 'task' }),
  node('p.py:orphan', [], failedAt('2026-10-29 12:00:00', 'ZeroDivisionError: division by zero')),
]
const byId = new Map(nodes.map((n) => [n.id, n]))
const consumers = consumersOf(nodes)
// The roots `GET /state/rollup` serves for them: `raw` holds itself and `clean`.
const roots = [failureRoot('p.py:raw', "KeyError: 'currency'", 2), failureRoot('p.py:orphan', 'ZeroDivisionError: division by zero')]

describe('health', () => {
  it('lists the failure roots as served, each with its row and how many nodes it holds', () => {
    const list = failures(roots, byId)
    expect(list.map((f) => [f.name, f.held, f.heldAssets, f.headline, f.group])).toEqual([
      ['raw', 2, 2, "KeyError: 'currency'", "KeyError: 'currency'"],
      ['orphan', 1, 1, 'ZeroDivisionError: division by zero', 'ZeroDivisionError: division by zero'],
    ])
    expect(list[0]).toMatchObject({ at: '2026-10-29 11:00:00', error: "KeyError: 'currency'\n  File \"p.py\", line 3" })
    // A root whose row has not loaded is left out until it has.
    expect(failures([failureRoot('p.py:gone', 'x'), ...roots], byId).map((f) => f.name)).toEqual(['raw', 'orphan'])
  })

  it('says which failing node blocks a row from the row itself', () => {
    expect(blockerOf(byId.get('p.py:clean')!)).toBe('p.py:raw')
    // A failing node's root is itself: it is failing, not blocked.
    expect(blockerOf(byId.get('p.py:raw')!)).toBeNull()
    expect(blockerOf(byId.get('p.py:report')!)).toBeNull()
    expect([...downstreamIds('p.py:raw', consumers)].sort()).toEqual(['p.py:check', 'p.py:clean', 'p.py:report'])
  })

  it('says something for an empty error', () => {
    expect(headline('\n\n  ValueError: x\n trace')).toBe('ValueError: x')
    expect(headline(null)).toBe('failed, with no error recorded')
  })
})

describe('the failures of the generated project', () => {
  const rows = new Map(STATE.map((n) => [n.id, n]))

  it('are its failure roots, each with its row, its streak and what it holds', () => {
    const list = failures(ROLLUP.roots, rows)
    expect(list.map((f) => [f.name, f.kind, f.headline, f.held, f.heldAssets, f.failures, f.failingSince])).toEqual([
      ['invoices', 'asset', 'ValueError: 3 rows with a negative total', 3, 3, 2, '2026-11-17 00:40:01'],
      ['sales_by_day', 'asset', "KeyError: 'units'", 2, 2, 1, '2026-11-17 02:00:01'],
      ['check__invoices', 'task', 'AssertionError: 3 rows with a negative total', 1, 0, 1, '2026-11-16 10:00:01'],
    ])
    // The server groups errors with their numbers taken out; the two "3 rows" errors differ by type.
    expect(list.map((f) => f.group)).toEqual(['ValueError: # rows with a negative total', "KeyError: 'units'", 'AssertionError: # rows with a negative total'])
    // One key of five failed, and one more was never computed.
    expect(list[1]).toMatchObject({ keys: { failed: 1, total: 5, errors: null, runId: null }, missingKeys: 2 })
    expect(keysLine(list[1]!.keys!)).toBe('1 of 5 keys')
    expect(failedKeyNodes(STATE)).toEqual(['orders.py:sales_by_day'])
  })

  it('name the failing node on every row it blocks, and on no other', () => {
    expect(STATE.map((n) => [n.name, blockerOf(n)]).filter(([, by]) => by !== null)).toEqual([
      ['invoice_totals', 'billing.py:invoices'],
      ['ledger', 'billing.py:invoices'],
      ['sales_report', 'orders.py:sales_by_day'],
    ])
    // A blocked row is served as held by the failure: its cause is `failing`, like the node that failed.
    expect(['invoice_totals', 'ledger', 'sales_report'].map((name) => [row(name).health, row(name).health_reason, row(name).cause])).toEqual(Array(3).fill(['blocked', 'upstream_failing', 'failing']))
    // A task that reads the failing node is failing on its own account, not blocked by it.
    expect(blockerOf(row('check__invoices'))).toBeNull()
    expect([...downstreamIds('billing.py:invoices', consumersOf(STATE))].sort()).toEqual(['billing.py:check__invoices', 'billing.py:invoice_totals', 'billing.py:ledger'])
  })
})

describe('streakLabel', () => {
  it('says a failing streak in runs in a row, never as a total', () => {
    expect(streakLabel(1)).toBe('1 failed run')
    expect(streakLabel(130)).toBe('130 failed runs in a row')
    expect(streakLabel(1205)).toBe('1,205 failed runs in a row')
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
    keys: [{ key: 'account=ra-02541', status: 'failed', created_at: '2026-10-31 15:30:12', elapsed_seconds: null, error: SAMPLE, run_id: 'd9becbdf4a92', artifact_path: null, size_bytes: null, rows: null }],
    errors: [{ error: KEY_ERROR, keys: 5 }, { error: 'KeyError: <key>', keys: 2 }],
  }
  const forecast = (over: Partial<NodeState> = {}) =>
    node('p.py:forecast', [], {
      partitioned: true,
      cache: { state: 'stale', reason: 'failed', detail: '' },
      partitions: { total: 3077, cached: 3070, missing: 7, missing_keys: [], failed: 7, failed_keys: [], behind: 0 },
      ...over,
    })
  const loaded = new Map([['p.py:forecast', page]])
  // Its root as served: the node's own error line, or how many keys failed when it has none.
  const keysRoot = { ...failureRoot('p.py:forecast', '7 of 3077 keys failed'), error_group: null }
  const errorRoot = (detail: string) => failureRoot('p.py:forecast', detail)
  const one = (n: NodeState) => new Map([[n.id, n]])

  it('counts the failed keys until their errors are loaded, then heads with the most common', () => {
    const [before] = failures([keysRoot], new Map([['p.py:forecast', forecast()]]))
    expect(before).toMatchObject({ headline: '7 of 3,077 keys failed', error: '', keys: { failed: 7, total: 3077, errors: null, runId: null } })
    expect(keysLine(before!.keys!)).toBe('7 of 3,077 keys')
    const [after] = failures([keysRoot], new Map([['p.py:forecast', forecast()]]), loaded)
    expect(after).toMatchObject({ headline: KEY_ERROR, error: SAMPLE, keys: { failed: 7, total: 3077, errors: page.errors, runId: 'd9becbdf4a92' } })
    expect(keysLine(after!.keys!)).toBe('7 of 3,077 keys · +1 more error')
  })

  it('a key error outranks the node-level one, which is one key\'s', () => {
    const withError = forecast(failedAt('2026-10-31 15:30:12', 'BatchKeyFailed: forecast model diverged for ra-02541: NaN in week 14'))
    const root = errorRoot('BatchKeyFailed: forecast model diverged for ra-02541: NaN in week 14')
    expect(failures([root], one(withError))[0]!.headline).toBe('BatchKeyFailed: forecast model diverged for ra-02541: NaN in week 14')
    expect(failures([root], one(withError), loaded)[0]!.headline).toBe(KEY_ERROR)
  })

  it('leaves a node whose keys did not fail alone', () => {
    // Failed as a whole, with 7 keys stale rather than failed: its own error, and no key errors to load.
    const whole = forecast({ ...failedAt('2026-10-31 15:30:12', 'ImportError: no module named prophet'), partitions: { total: 3077, cached: 3070, missing: 7, missing_keys: [], behind: 7 } })
    expect(failures([errorRoot('ImportError: no module named prophet')], one(whole))[0]).toMatchObject({ headline: 'ImportError: no module named prophet', keys: null, missingKeys: 7 })
    expect(failedKeyNodes([forecast(), whole, ...nodes])).toEqual(['p.py:forecast'])
  })
})

describe('relativeTraceback', () => {
  it('takes the project root off paths', () => {
    expect(relativeTraceback('File "/srv/p/a/b.py", line 3', '/srv/p')).toBe('File "a/b.py", line 3')
    expect(relativeTraceback('File "/x/b.py"', null)).toBe('File "/x/b.py"')
  })
  it('splits after separators', () => {
    expect(pathBreaks('/a/b.py')).toEqual(['/', 'a/', 'b.py'])
  })
})
