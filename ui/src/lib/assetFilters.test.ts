import { describe, expect, it } from 'vitest'
import { served } from './testdata/served'
import type { CacheStatus, NodeState } from './types'
import { buildRows } from './assetTable'
import { nodeStateDisplay } from './cacheStateDisplay'
import { blockedBy } from './testdata/served'
import { causeCounts } from './testdata/rollup'
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
  return served({
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
          rows: null,
          error: failed ? 'boom' : null,
        }
      : null,
    shape: null,
    env: [],
    durations: null,
    next_run: null, prev_run: null, late_since: null, signals: [], last_success_at: null, failing_since: null, consecutive_failures: 0,
    running: null,
    ...rest,
  })
}

// Each row states the cause the server gave it (`cause`); a fresh row has none.
const rows = buildRows(
  [
    node('p.py:recent', { lastAt: '2026-10-06 11:30:00' }),
    node('p.py:today', { lastAt: '2026-10-06 02:00:00', state: 'stale', cause: 'catching_up' }),
    node('p.py:old', { lastAt: '2026-09-20 12:00:00' }),
    node('p.py:never', { state: 'never_run', cause: 'no_refresh_path' }),
    node('p.py:check', { kind: 'task', state: 'always_runs', lastAt: '2026-10-06 11:50:00', failed: true, cause: 'failing' }),
    node('p.py:watch', { kind: 'sensor', state: 'always_runs', next_run: NOW / 1000 + 60 }),
    node('p.py:parts', { partitioned: true, state: 'partial', lastAt: '2026-10-06 09:00:00', cause: 'overdue' }),
  ],
  NOW,
)
const names = (f: Filters) => applyFilters(rows, f, NOW).map((r) => r.name).sort()

describe('applyFilters', () => {
  it('no filters keeps everything', () => {
    expect(applyFilters(rows, EMPTY_FILTERS, NOW)).toHaveLength(rows.length)
    expect(isFiltered(EMPTY_FILTERS)).toBe(false)
  })

  it("the state chips are the health map's parts: a row falls under its served cause, or fresh", () => {
    expect(names({ ...EMPTY_FILTERS, states: ['catching_up'] })).toEqual(['today'])
    expect(names({ ...EMPTY_FILTERS, states: ['failing'] })).toEqual(['check'])
    expect(names({ ...EMPTY_FILTERS, states: ['catching_up', 'failing'] })).toEqual(['check', 'today'])
    expect(names({ ...EMPTY_FILTERS, states: ['fresh'] })).toEqual(['old', 'recent', 'watch'])
    // The cache state decides nothing: a `stale` row the server calls fresh is under fresh.
    const odd = buildRows([node('p.py:odd', { state: 'stale' })], NOW)
    expect(applyFilters(odd, { ...EMPTY_FILTERS, states: ['fresh'] }, NOW)).toHaveLength(1)
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
    // With tasks/sensors filtered out, the state chips count the assets on screen by cause…
    const c = facetCounts(rows, { ...EMPTY_FILTERS, kinds: ['asset'] }, NOW)
    expect(c.states).toEqual({ failing: 0, overdue: 1, no_refresh_path: 1, queued: 0, undecided: 0, catching_up: 1, fresh: 2 })
    // …but the kind chips still show what picking another kind would give.
    expect(c.kinds).toEqual({ asset: 5, task: 1, sensor: 1 })
    expect(c.scheduled).toBe(0)
    expect(c.partitioned).toBe(1)
  })

  it("reads an unfiltered page's counts from the rollup, not from its rows", () => {
    // What `kinds.asset` of `GET /state/rollup` states for the page: deliberately not what
    // these rows would add up to, so the number shown can only be the server's.
    const served = { nodes: 602, fresh: 180, causes: causeCounts({ failing: 42, no_refresh_path: 173, catching_up: 207 }) }
    const c = facetCounts(rows, EMPTY_FILTERS, NOW, served)
    expect(c.states).toEqual({ failing: 42, overdue: 0, no_refresh_path: 173, queued: 0, undecided: 0, catching_up: 207, fresh: 180 })
    // Picking a chip narrows nothing its own group counts: still the rollup's.
    expect(facetCounts(rows, { ...EMPTY_FILTERS, states: ['failing'] }, NOW, served).states.catching_up).toBe(207)
    // Under a search (or any other filter) a count is the rows on screen that carry the cause.
    expect(facetCounts(rows, { ...EMPTY_FILTERS, query: 'to' }, NOW, served).states).toMatchObject({ catching_up: 1, failing: 0, fresh: 0 })
    expect(facetCounts(rows, { ...EMPTY_FILTERS, partitioned: true }, NOW, served).states).toMatchObject({ overdue: 1, fresh: 0 })
    // Without the rollup (not loaded yet) the rows are what there is.
    expect(facetCounts(rows, EMPTY_FILTERS, NOW, null).states.fresh).toBe(3)
  })
})

describe('URL round-trip', () => {
  it('writes only what is set and reads it back', () => {
    const f: Filters = {
      query: 'ibp',
      states: ['failing', 'no_refresh_path'],
      pinned: true,
      kinds: ['asset'],
      lastRun: 'week_plus',
      scheduled: true,
      partitioned: false,
    }
    const p = writeFilters(new URLSearchParams('pipeline=x.py&sort=name'), f)
    expect(p.get('pipeline')).toBe('x.py') // other params are kept
    expect(p.get('partitioned')).toBeNull()
    expect(p.get('state')).toBe('failing,no_refresh_path')
    expect(parseFilters(p)).toEqual(f)
    expect(parseFilters(writeFilters(new URLSearchParams(), EMPTY_FILTERS))).toEqual(EMPTY_FILTERS)
  })

  it('ignores values it does not know', () => {
    const p = new URLSearchParams('state=catching_up,stale,bogus&kind=widget&ran=sometime')
    expect(parseFilters(p)).toEqual({ ...EMPTY_FILTERS, states: ['catching_up'] })
  })

  it("reads an older link's failed=1, blocked=1 and state=cached as the parts they are now", () => {
    expect(parseFilters(new URLSearchParams('failed=1')).states).toEqual(['failing'])
    expect(parseFilters(new URLSearchParams('blocked=1&state=failing')).states).toEqual(['failing'])
    expect(parseFilters(new URLSearchParams('state=cached')).states).toEqual(['fresh'])
    // And writes them back in the new words only.
    expect(writeFilters(new URLSearchParams('failed=1&blocked=1'), parseFilters(new URLSearchParams('failed=1'))).toString()).toBe('state=failing')
  })
})

describe('a page opens unfiltered', () => {
  it('filters nothing until the URL names a filter, and writes nothing for none', () => {
    expect(parseFilters(new URLSearchParams())).toEqual(EMPTY_FILTERS)
    expect(isFiltered(parseFilters(new URLSearchParams()))).toBe(false)
    expect(writeFilters(new URLSearchParams('pipeline=a.py'), EMPTY_FILTERS).toString()).toBe('pipeline=a.py')
  })
})

describe('failing rows and what they block', () => {
  // broken fails; report never ran because of it; summary waits on report; other just never ran.
  // Which node blocks which, and every cause, is the server's answer on each row.
  const nodes = [
    node('p:broken', { lastAt: '2026-10-06 11:00:00', failed: true, cause: 'failing' }),
    node('p:report', { state: 'never_run', inputs: ['p:broken'], ...blockedBy('p:broken'), cause: 'failing' }),
    node('p:summary', { state: 'stale', inputs: ['p:report'], ...blockedBy('p:broken'), cause: 'failing' }),
    node('p:other', { state: 'never_run', cause: 'no_refresh_path' }),
  ]
  const rows = buildRows(nodes, NOW)

  it('names the failure upstream on a blocked row, and a failing node stays failing', () => {
    const by = Object.fromEntries(rows.map((r) => [r.name, r.blockedBy]))
    expect(by).toEqual({ broken: null, report: 'p:broken', summary: 'p:broken', other: null })
    expect(nodeStateDisplay(rows.find((r) => r.name === 'report')!)).toEqual({ label: 'blocked', tone: 'warning', glyph: 'blocked' })
  })

  it('is one chip: a failing node and what it blocks have the failing cause', () => {
    const c = facetCounts(rows, EMPTY_FILTERS, NOW)
    expect([c.states.failing, c.states.no_refresh_path, c.states.fresh]).toEqual([3, 1, 0])
    expect(applyFilters(rows, { ...EMPTY_FILTERS, states: ['failing'] }, NOW).map((r) => r.name).sort()).toEqual(['broken', 'report', 'summary'])
  })

  it('sorts in the served order of the causes, then by name', () => {
    expect(rows.map((r) => r.name)).toEqual(['broken', 'report', 'summary', 'other'])
  })
})

describe('pinned rows', () => {
  // report is pinned (stale) below a failure; fixed is pinned and failing.
  const nodes = [
    node('p:broken', { lastAt: '2026-10-06 11:00:00', failed: true, cause: 'failing' }),
    node('p:report', { state: 'stale', inputs: ['p:broken'], cause: 'no_refresh_path' }),
    node('p:summary', { state: 'stale', inputs: ['p:report'], cause: 'catching_up' }),
    node('p:fixed', { lastAt: '2026-10-06 11:00:00', failed: true, cause: 'failing' }),
  ]
  const pins = new Map([['p:report', 'override from fix.json by me'], ['p:fixed', 'rolled back to abc']])
  const rows = buildRows(nodes, NOW, pins)
  const row = (name: string) => rows.find((r) => r.name === name)!

  it('reads pinned, and passes no failure above it on; failing still outranks it', () => {
    expect(nodeStateDisplay(row('report'))).toEqual({ label: 'pinned', tone: 'warning', glyph: 'pinned' })
    expect(row('report').pinned).toBe('override from fix.json by me')
    expect(row('report').blockedBy).toBeNull()
    expect(row('summary').blockedBy).toBeNull()
    expect(nodeStateDisplay(row('fixed')).label).toBe('failing')
  })

  it('has a chip and a ?pinned=1 filter of its own, beside the cause chips', () => {
    const c = facetCounts(rows, EMPTY_FILTERS, NOW)
    expect(c.pinned).toBe(2)
    expect(c.states.failing).toBe(2)
    expect(applyFilters(rows, parseFilters(new URLSearchParams('pinned=1')), NOW).map((r) => r.name).sort()).toEqual(['fixed', 'report'])
    // The two combine: pinned and failing.
    expect(applyFilters(rows, parseFilters(new URLSearchParams('pinned=1&state=failing')), NOW).map((r) => r.name)).toEqual(['fixed'])
    expect(facetCounts(rows, parseFilters(new URLSearchParams('state=failing')), NOW).pinned).toBe(1)
  })

  it('sorts by cause like any row: a pin is a fact beside it', () => {
    expect(rows.map((r) => r.name)).toEqual(['broken', 'fixed', 'report', 'summary'])
  })
})
