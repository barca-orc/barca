import { describe, expect, it } from 'vitest'
import type { CacheStatus, NodeState } from './types'
import { CACHE_STATES } from './cacheStateDisplay'
import {
  DEFAULT_SORT,
  buildRows,
  filterRows,
  formatAgo,
  formatSeconds,
  nextSort,
  parseSort,
  sortRows,
  summarize,
  typicalLabel,
  typicalSeconds,
  type Sort,
} from './assetTable'

const NOW = Date.parse('2026-10-02T12:00:00Z')

/** The single row of a one-node table. */
function only<T>(xs: T[]): T {
  expect(xs).toHaveLength(1)
  return xs[0]!
}

function cache(
  state: CacheStatus['state'],
  reason: CacheStatus['reason'] = 'materialized',
): CacheStatus {
  return { state, reason, detail: `${state} because ${reason}` }
}

function node(over: Partial<NodeState> & { id: string }): NodeState {
  return {
    name: over.id.split(':').pop()!,
    kind: 'asset',
    inputs: [],
    partitioned: false,
    cache: cache('cached'),
    last_materialization: null,
    shape: null,
    env: [],
    durations: null,
    next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0,
    running: null,
    ...over,
  }
}

const failedAttempt = {
  status: 'failed',
  created_at: '2026-10-02 11:24:51',
  elapsed_seconds: 0.2,
  run_hash: null,
  artifact: null,
  format: null,
  size_bytes: null,
  error: 'AssertionError: 3 rows with negative units',
}

describe('rows carry the state as the server says it', () => {
  it('shows every cache state in its own word, never another', () => {
    for (const state of CACHE_STATES) {
      const row = only(buildRows([node({ id: 'a', cache: cache(state, 'no_record') })], NOW))
      expect(row.state).toBe(state)
      expect(row.lastFailed).toBe(false)
    }
    // A task keeps `always_runs` whatever its last run did: the run is a second fact.
    const task = (status: string) => only(buildRows([node({ id: 't', kind: 'task', cache: cache('always_runs', 'task'), last_materialization: { ...failedAttempt, status, error: null } })], NOW))
    expect(task('success')).toMatchObject({ state: 'always_runs', lastFailed: false })
    expect(task('cancelled')).toMatchObject({ state: 'always_runs', lastFailed: false })
    expect(task('failed')).toMatchObject({ state: 'always_runs', lastFailed: true })
    // Blocked is blocked, not stale.
    expect(only(buildRows([node({ id: 'a', cache: cache('blocked', 'upstream_failed') })], NOW)).state).toBe('blocked')
  })

  it('a failed latest attempt is a fact beside the state, not in its place', () => {
    const row = only(buildRows([node({ id: 'a', last_materialization: failedAttempt })], NOW))
    expect(row.state).toBe('cached')
    expect(row.lastFailed).toBe(true)
    expect(row.reason).toBe('materialized')
  })
})

describe('buildRows', () => {
  it("sorts a failed last attempt first, then the state's rank, then name", () => {
    const rows = buildRows(
      [
        node({ id: 'p.py:zeta' }),
        node({ id: 'p.py:alpha' }),
        node({ id: 'p.py:new_one', cache: cache('never_run', 'no_record') }),
        node({
          id: 'p.py:broken',
          kind: 'task',
          cache: cache('always_runs', 'task'),
          last_materialization: failedAttempt,
        }),
        node({ id: 'p.py:edited', cache: cache('stale', 'changed') }),
      ],
      NOW,
    )
    expect(rows.map((r) => r.name)).toEqual(['broken', 'edited', 'new_one', 'alpha', 'zeta'])
  })

  it('describes state, last attempt, timing and schedule', () => {
    const row = only(
      buildRows(
        [
          node({
            id: 'utz/assets.py:ibp_model',
            cache: {
              state: 'stale',
              reason: 'upstream_stale',
              detail: "upstream 'dim_ppg' will re-run",
            },
            last_materialization: {
              ...failedAttempt,
              status: 'success',
              created_at: '2026-10-02 11:55:00',
              elapsed_seconds: 3.2,
              error: null,
            },
            durations: { median_seconds: 3.04, p95_seconds: 9.5, samples: 12 },
            next_run: Date.parse('2026-10-03T06:00:00Z') / 1000,
          }),
        ],
        NOW,
      ),
    )
    expect(row.name).toBe('ibp_model')
    expect(row.file).toBe('utz/assets.py')
    expect(row.state).toBe('stale')
    // The reason and the explanation come from the server, verbatim, not the UI.
    expect(row.reason).toBe('upstream_stale')
    expect(row.stateHint).toBe("upstream 'dim_ppg' will re-run")
    expect(row.last).toEqual({ status: 'success', ago: '5m ago', error: null })
    expect(row.typical).toBe('3.0s')
    expect(row.p95).toBe('9.5s')
    expect(row.nextRunMs).toBe(Date.parse('2026-10-03T06:00:00Z'))
  })

  it('keeps each reason verbatim: the UI does not rename them', () => {
    const reason = (r: CacheStatus['reason']) => only(buildRows([node({ id: 'p.py:a', cache: cache('stale', r) })], NOW)).reason
    for (const r of ['changed', 'upstream_stale', 'sensor_output_unknown', 'cutoff_pending'] as const) expect(reason(r)).toBe(r)
    // Keys waiting on failed upstream keys (#764) are `blocked`, with the server's detail as the hint.
    const blocked = only(
      buildRows(
        [
          node({
            id: 'p.py:promo_plan',
            partitioned: true,
            cache: { state: 'blocked', reason: 'upstream_failed', detail: '39 keys wait on upstream' },
            partitions: { total: 3077, cached: 3038, missing: 39, missing_keys: [] },
          }),
        ],
        NOW,
      ),
    )
    expect(blocked).toMatchObject({ state: 'blocked', reason: 'upstream_failed', stateHint: '39 keys wait on upstream' })
  })

  it('reports partition progress and never-run nodes', () => {
    const rows = buildRows(
      [
        node({
          id: 'p.py:fetch',
          partitioned: true,
          cache: cache('partial', 'partitions_missing'),
          partitions: { total: 5, cached: 3, missing: 2, missing_keys: ['k=d', 'k=e'] },
        }),
        node({ id: 'p.py:new', cache: cache('never_run', 'no_record') }),
      ],
      NOW,
    )
    const fetch = rows.find((r) => r.name === 'fetch')!
    const fresh = rows.find((r) => r.name === 'new')!
    expect(fetch.state).toBe('partial')
    expect(fresh.state).toBe('never_run')
    expect(fresh.last).toBeNull()
    expect(fresh.typical).toBeNull()
  })

  it('surfaces the error of a failed attempt', () => {
    const row = only(
      buildRows(
        [
          node({
            id: 'p.py:v',
            kind: 'task',
            cache: cache('always_runs', 'task'),
            last_materialization: failedAttempt,
          }),
        ],
        NOW,
      ),
    )
    expect(row.state).toBe('always_runs')
    expect(row.lastFailed).toBe(true)
    expect(row.last?.error).toBe('AssertionError: 3 rows with negative units')
    expect(row.last?.ago).toBe('35m ago')
  })
})

describe('typical time', () => {
  it('is the median of a plain node', () => {
    const n = node({ id: 'p.py:a', durations: { median_seconds: 3.04, p95_seconds: 9.5, samples: 12 } })
    expect(typicalSeconds(n)).toBe(3.04)
    expect(typicalLabel(n)).toBe('3.0s')
  })
  it('is a run over every key of a partitioned node, never a per-key figure', () => {
    // Batched calls record a few milliseconds per key: 0.0007s × 3,077 keys is the 2.2s run.
    const n = node({
      id: 'p.py:forecast',
      partitioned: true,
      durations: { median_seconds: 0.0007, p95_seconds: 0.0009, samples: 20 },
      partitions: { total: 3077, cached: 3070, missing: 7, missing_keys: [], failed: 7 },
    })
    expect(typicalSeconds(n)).toBeCloseTo(2.154, 3)
    expect(typicalLabel(n)).toBe('~2.2s / run')
    expect(only(buildRows([n], NOW)).typical).toBe('~2.2s / run')
  })
  it('is null until the node has succeeded', () => {
    expect(typicalLabel(node({ id: 'p.py:a' }))).toBeNull()
  })
})

describe('filterRows', () => {
  const rows = buildRows(
    [node({ id: 'utz/assets.py:ibp_model' }), node({ id: 'utz/assets.py:dim_ppg' })],
    NOW,
  )
  it('matches name or id, case-insensitively', () => {
    expect(filterRows(rows, 'IBP').map((r) => r.name)).toEqual(['ibp_model'])
    expect(filterRows(rows, 'utz/').length).toBe(2)
  })
  it('an empty query keeps everything', () => {
    expect(filterRows(rows, '  ').length).toBe(2)
  })
})

describe('summarize', () => {
  it('counts every state, including zeroes, and failed last attempts beside them', () => {
    const rows = buildRows(
      [node({ id: 'a' }), node({ id: 'b', last_materialization: failedAttempt }), node({ id: 'c', cache: cache('stale', 'changed') })],
      NOW,
    )
    expect(summarize(rows)).toEqual({
      states: { blocked: 0, stale: 1, partial: 0, unsynced: 0, unknown: 0, never_run: 0, always_runs: 0, cached: 2 },
      lastFailed: 1,
    })
  })
})

describe('formatting', () => {
  it('formats seconds compactly', () => {
    expect(formatSeconds(0.04)).toBe('0.0s')
    expect(formatSeconds(12.34)).toBe('12.3s')
    expect(formatSeconds(125)).toBe('2.1m')
    expect(formatSeconds(7200)).toBe('2.0h')
  })
  it('formats UTC timestamps relative to now', () => {
    expect(formatAgo('2026-10-02 11:59:30', NOW)).toBe('just now')
    expect(formatAgo('2026-10-02 10:00:00', NOW)).toBe('2h ago')
    expect(formatAgo('2026-09-29 12:00:00', NOW)).toBe('3d ago')
  })
})

describe('sorting', () => {
  const rows = buildRows(
    [
      node({
        id: 'p.py:slow',
        durations: { median_seconds: 9, p95_seconds: 12, samples: 3 },
        last_materialization: { ...failedAttempt, status: 'success', error: null, created_at: '2026-10-02 09:00:00' },
      }),
      node({
        id: 'p.py:fast',
        durations: { median_seconds: 0.5, p95_seconds: 1, samples: 3 },
        last_materialization: { ...failedAttempt, status: 'success', error: null, created_at: '2026-10-02 11:00:00' },
        next_run: Date.parse('2026-10-03T06:00:00Z') / 1000,
      }),
      node({ id: 'p.py:never', cache: cache('never_run', 'no_record') }),
      node({
        id: 'p.py:edited',
        cache: cache('stale', 'changed'),
        durations: { median_seconds: 3, p95_seconds: 4, samples: 2 },
        next_run: Date.parse('2026-10-02T13:00:00Z') / 1000,
      }),
    ],
    NOW,
  )
  const names = (sort: Sort) => sortRows(rows, sort).map((r) => r.name)

  it('defaults to what needs attention, most urgent first', () => {
    expect(DEFAULT_SORT).toEqual({ key: 'state', dir: 'asc' })
    expect(names(DEFAULT_SORT)).toEqual(['edited', 'never', 'fast', 'slow'])
  })

  it('sorts by name either way', () => {
    expect(names({ key: 'name', dir: 'asc' })).toEqual(['edited', 'fast', 'never', 'slow'])
    expect(names({ key: 'name', dir: 'desc' })).toEqual(['slow', 'never', 'fast', 'edited'])
  })

  it('rows without a value go last in both directions', () => {
    // `never` has no typical time and `edited` no last run: they never jump to the top.
    expect(names({ key: 'typical', dir: 'desc' })).toEqual(['slow', 'edited', 'fast', 'never'])
    expect(names({ key: 'typical', dir: 'asc' })).toEqual(['fast', 'edited', 'slow', 'never'])
    expect(names({ key: 'last', dir: 'desc' }).slice(0, 2)).toEqual(['fast', 'slow'])
    expect(names({ key: 'last', dir: 'asc' }).slice(0, 2)).toEqual(['slow', 'fast'])
    expect(names({ key: 'next', dir: 'asc' }).slice(0, 2)).toEqual(['edited', 'fast'])
  })

  it('a first click picks the useful direction; a second reverses it', () => {
    expect(nextSort(DEFAULT_SORT, 'typical')).toEqual({ key: 'typical', dir: 'desc' })
    expect(nextSort(DEFAULT_SORT, 'last')).toEqual({ key: 'last', dir: 'desc' })
    expect(nextSort(DEFAULT_SORT, 'next')).toEqual({ key: 'next', dir: 'asc' })
    expect(nextSort(DEFAULT_SORT, 'name')).toEqual({ key: 'name', dir: 'asc' })
    expect(nextSort({ key: 'typical', dir: 'desc' }, 'typical')).toEqual({ key: 'typical', dir: 'asc' })
    expect(nextSort(DEFAULT_SORT, 'state')).toEqual({ key: 'state', dir: 'desc' })
  })

  it('reads the sort from the URL, falling back to the default', () => {
    expect(parseSort('typical', 'desc')).toEqual({ key: 'typical', dir: 'desc' })
    expect(parseSort('typical', null)).toEqual({ key: 'typical', dir: 'desc' })
    expect(parseSort('bogus', 'asc')).toEqual(DEFAULT_SORT)
    expect(parseSort(null, null)).toEqual(DEFAULT_SORT)
  })
})
