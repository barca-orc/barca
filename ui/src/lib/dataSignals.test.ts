import { describe, expect, it } from 'vitest'
import { coverageLine, dataCoverage, dataSignalSets, signalLine } from './dataSignals'
import { served } from './testdata/served'
import type { NodeState } from './types'

describe('signalLine', () => {
  it('says each signal in one line, with its numbers', () => {
    expect(signalLine({ kind: 'empty', empty_keys: 3, keys: 1205, previous_empty_keys: 0 })).toBe('3 of 1,205 keys empty, previously 0')
    expect(signalLine({ kind: 'empty', empty_keys: 2, keys: 4, previous_empty_keys: null })).toBe('2 of 4 keys empty')
    expect(signalLine({ kind: 'empty', empty_keys: 1, keys: 1, previous_empty_keys: 0 })).toBe('empty output (had rows before)')
    expect(signalLine({ kind: 'rows_changed', changed_keys: 1, key: null, rows: 300, previous_rows: 1200 })).toBe('rows changed: 1,200 rows → 300')
    expect(signalLine({ kind: 'rows_changed', changed_keys: 4, key: 'region=r1', rows: 0, previous_rows: 1 })).toBe('rows changed on region=r1: 1 row → 0 (4 keys changed)')
    expect(
      signalLine({ kind: 'behind_input', input: 'p.py:orders', input_success_at: '2026-11-16 09:00:00', last_success_at: '2026-11-14 10:00:00' }),
    ).toBe('older than its input orders')
  })
})

const row = (id: string, over: Partial<NodeState> = {}): NodeState =>
  served({
    id, name: id, kind: 'asset', inputs: [], partitioned: false, cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: null, shape: null, env: [], durations: null, next_run: null, prev_run: null, late_since: null, signals: [],
    running: null, last_success_at: null, failing_since: null, consecutive_failures: 0, ...over,
  })

describe('data coverage', () => {
  it('says how many assets were measured, and never calls an unmeasured one fine', () => {
    const states = [
      row('p.py:a', { measured: true }),
      row('p.py:b', { measured: true, signals: [{ kind: 'empty', empty_keys: 1, keys: 1, previous_empty_keys: 0 }] }),
      row('p.py:c'),
      row('p.py:t', { kind: 'task' }),
    ]
    expect(dataCoverage(states)).toEqual({ assets: 3, measured: 2, suspect: 1 })
    expect(coverageLine(dataCoverage(states))).toBe('2 of 3 measured · 1 suspect')
    expect(coverageLine({ assets: 556, measured: 7, suspect: 0 })).toBe('7 of 556 measured')
    expect(coverageLine({ assets: 3, measured: 0, suspect: 0 })).toBe('0 of 3 measured')
  })
})

describe('dataSignalSets', () => {
  it('words the signals and the lateness the server reports, and decides neither', () => {
    const now = Date.parse('2026-11-16T12:17:00Z')
    const sets = dataSignalSets(
      [
        row('p.py:empty', { signals: [{ kind: 'empty', empty_keys: 1, keys: 1, previous_empty_keys: null }] }),
        row('p.py:fan', { signals: [{ kind: 'behind_input', input: 'p.py:orders', input_success_at: '2026-11-16 09:00:00', last_success_at: '2026-11-14 10:00:00' }] }),
        row('p.py:job', { health: 'behind', health_reason: 'schedule_late', late_since: Date.parse('2026-11-16T12:05:00Z') / 1000 }),
        // A missed slot the server does not call late (within its grace, or paused) is not behind.
        row('p.py:soon', { late_since: Date.parse('2026-11-16T12:16:30Z') / 1000 }),
      ],
      now,
    )
    expect([...sets.suspect]).toEqual([['p.py:empty', 'empty output']])
    expect([...sets.behind]).toEqual([
      ['p.py:fan', 'older than its input orders'],
      ['p.py:job', 'schedule missed its slot: late by 12m'],
    ])
  })
})
