import { describe, expect, it } from 'vitest'
import type { CacheStatus, NodeState } from './types'
import { buildRows } from './assetTable'
import {
  EMPTY_FILTERS,
  applyFilters,
  facetCounts,
  isFiltered,
  parseFilters,
  writeFilters,
  type Filters,
} from './assetFilters'

const NOW = Date.parse('2026-10-06T12:00:00Z')

function node(
  id: string,
  over: Partial<NodeState> & { state?: CacheStatus['state']; lastAt?: string; failed?: boolean } = {},
): NodeState {
  const { state = 'cached', lastAt, failed = false, ...rest } = over
  return {
    id,
    name: id.split(':').pop()!,
    kind: 'asset',
    inputs: [],
    partitioned: false,
    cache: { state, reason: 'materialized', detail: '' },
    last_materialization: lastAt
      ? {
          status: failed ? 'failed' : 'success',
          created_at: lastAt,
          elapsed_seconds: 1,
          run_hash: null,
          artifact: null,
          format: null,
          size_bytes: null,
          error: failed ? 'boom' : null,
        }
      : null,
    shape: null,
    env: [],
    durations: null,
    next_run: null,
    ...rest,
  }
}

const rows = buildRows(
  [
    node('p.py:recent', { lastAt: '2026-10-06 11:30:00' }),
    node('p.py:today', { lastAt: '2026-10-06 02:00:00', state: 'stale' }),
    node('p.py:old', { lastAt: '2026-09-20 12:00:00' }),
    node('p.py:never', { state: 'never_run' }),
    node('p.py:check', { kind: 'task', state: 'always_runs', lastAt: '2026-10-06 11:50:00', failed: true }),
    node('p.py:watch', { kind: 'sensor', state: 'always_runs', next_run: NOW / 1000 + 60 }),
    node('p.py:parts', { partitioned: true, state: 'partial', lastAt: '2026-10-06 09:00:00' }),
  ],
  NOW,
)
const names = (f: Filters) => applyFilters(rows, f, NOW).map((r) => r.name).sort()

describe('applyFilters', () => {
  it('no filters keeps everything', () => {
    expect(applyFilters(rows, EMPTY_FILTERS, NOW)).toHaveLength(rows.length)
    expect(isFiltered(EMPTY_FILTERS)).toBe(false)
  })

  it('states are multi-select (any of them)', () => {
    expect(names({ ...EMPTY_FILTERS, states: ['failed', 'stale'] })).toEqual(['check', 'today'])
  })

  it('kinds are multi-select', () => {
    expect(names({ ...EMPTY_FILTERS, kinds: ['task', 'sensor'] })).toEqual(['check', 'watch'])
  })

  it('last run windows', () => {
    expect(names({ ...EMPTY_FILTERS, lastRun: 'hour' })).toEqual(['check', 'recent'])
    expect(names({ ...EMPTY_FILTERS, lastRun: 'day' })).toEqual(['check', 'parts', 'recent', 'today'])
    expect(names({ ...EMPTY_FILTERS, lastRun: 'week_plus' })).toEqual(['old'])
    expect(names({ ...EMPTY_FILTERS, lastRun: 'never' })).toEqual(['never', 'watch'])
  })

  it('scheduled and partitioned', () => {
    expect(names({ ...EMPTY_FILTERS, scheduled: true })).toEqual(['watch'])
    expect(names({ ...EMPTY_FILTERS, partitioned: true })).toEqual(['parts'])
  })

  it('filters combine (all must hold), with search', () => {
    const f: Filters = { ...EMPTY_FILTERS, kinds: ['asset'], lastRun: 'day', query: 'to' }
    expect(names(f)).toEqual(['today'])
    expect(isFiltered(f)).toBe(true)
  })
})

describe('facetCounts', () => {
  it("counts each chip under every other active filter, ignoring its own group", () => {
    // With tasks/sensors filtered out, state counts only cover assets…
    const c = facetCounts(rows, { ...EMPTY_FILTERS, kinds: ['asset'] }, NOW)
    expect(c.states.failed).toBe(0)
    expect(c.states.stale).toBe(1)
    // …but the kind chips still show what picking another kind would give.
    expect(c.kinds).toEqual({ asset: 5, task: 1, sensor: 1 })
    expect(c.scheduled).toBe(0)
    expect(c.partitioned).toBe(1)
  })
})

describe('URL round-trip', () => {
  it('writes only what is set and reads it back', () => {
    const f: Filters = {
      query: 'ibp',
      states: ['failed', 'stale'],
      kinds: ['asset'],
      lastRun: 'week_plus',
      scheduled: true,
      partitioned: false,
    }
    const p = writeFilters(new URLSearchParams('pipeline=x.py&sort=name'), f)
    expect(p.get('pipeline')).toBe('x.py') // other params are kept
    expect(p.get('partitioned')).toBeNull()
    expect(parseFilters(p)).toEqual(f)
    expect(parseFilters(writeFilters(new URLSearchParams(), EMPTY_FILTERS))).toEqual(EMPTY_FILTERS)
  })

  it('ignores values it does not know', () => {
    const p = new URLSearchParams('state=failed,bogus&kind=widget&ran=sometime')
    expect(parseFilters(p)).toEqual({ ...EMPTY_FILTERS, states: ['failed'] })
  })
})
