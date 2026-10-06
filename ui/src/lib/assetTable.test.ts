import { describe, expect, it } from 'vitest'
import type { CacheStatus, NodeState } from './types'
import {
  buildRows,
  filterRows,
  formatAgo,
  formatSeconds,
  severityOf,
  summarize,
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
    next_run: null,
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

describe('severityOf', () => {
  it('maps every cache state', () => {
    expect(severityOf(node({ id: 'a', cache: cache('cached') }))).toBe('cached')
    expect(severityOf(node({ id: 'a', cache: cache('stale', 'changed') }))).toBe('stale')
    expect(severityOf(node({ id: 'a', cache: cache('never_run', 'no_record') }))).toBe('never_run')
    expect(severityOf(node({ id: 'a', cache: cache('partial', 'partitions_missing') }))).toBe(
      'partial',
    )
    expect(severityOf(node({ id: 'a', cache: cache('unknown', 'partitions_unknown') }))).toBe(
      'unknown',
    )
    expect(severityOf(node({ id: 'a', cache: cache('always_runs', 'task') }))).toBe('always_runs')
  })

  it('a failed latest attempt wins over any cache state', () => {
    // A task (always runs) whose last run failed is the thing to look at…
    const task = node({
      id: 't',
      kind: 'task',
      cache: cache('always_runs', 'task'),
      last_materialization: failedAttempt,
    })
    expect(severityOf(task)).toBe('failed')
    // …and so is a cached asset whose newer attempt failed without replacing it.
    expect(severityOf(node({ id: 'a', last_materialization: failedAttempt }))).toBe('failed')
  })
})

describe('buildRows', () => {
  it('sorts by severity, then name', () => {
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
    expect(row.stateLabel).toBe('stale · upstream')
    // The explanation comes from the server, not the UI.
    expect(row.stateHint).toBe("upstream 'dim_ppg' will re-run")
    expect(row.last).toEqual({ status: 'success', ago: '5m ago', error: null })
    expect(row.typical).toBe('3.0s')
    expect(row.p95).toBe('9.5s')
    expect(row.nextRunMs).toBe(Date.parse('2026-10-03T06:00:00Z'))
  })

  it('labels each reason a stale node can have', () => {
    const label = (reason: CacheStatus['reason']) =>
      only(buildRows([node({ id: 'p.py:a', cache: cache('stale', reason) })], NOW)).stateLabel
    expect(label('changed')).toBe('stale · code changed')
    expect(label('upstream_stale')).toBe('stale · upstream')
    expect(label('sensor_output_unknown')).toBe('stale · sensor')
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
    expect(fetch.stateLabel).toBe('partial · 3/5')
    expect(fresh.stateLabel).toBe('never run')
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
    expect(row.severity).toBe('failed')
    expect(row.stateLabel).toBe('failed · always runs')
    expect(row.last?.error).toBe('AssertionError: 3 rows with negative units')
    expect(row.last?.ago).toBe('35m ago')
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
  it('counts every severity, including zeroes', () => {
    const rows = buildRows(
      [node({ id: 'a' }), node({ id: 'b' }), node({ id: 'c', cache: cache('stale', 'changed') })],
      NOW,
    )
    expect(summarize(rows)).toEqual({
      failed: 0,
      stale: 1,
      never_run: 0,
      partial: 0,
      unknown: 0,
      always_runs: 0,
      cached: 2,
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
