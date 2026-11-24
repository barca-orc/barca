import { beforeAll, describe, expect, it } from 'vitest'
import { nodeEstimate, scopeEstimate } from './estimates'
import { NOW_MS as NOW, entry, row } from './testdata/project'
import { formatAbout, formatEstimate, formatSlot } from './time'

beforeAll(() => {
  process.env.TZ = 'America/New_York'
})

const node = (over: Partial<Parameters<typeof nodeEstimate>[0]> = {}) => ({ expected_fresh_at: null, recheck_at: null, no_estimate: null, cause: null, ...over })

describe('an estimate in words', () => {
  it('rounds an estimated time up to the minute and marks it with ~, never to the second', () => {
    expect(formatEstimate('2026-11-17T15:00:20Z', 'utc', NOW)).toBe('~15:01')
    expect(formatEstimate('2026-11-17T15:00:00Z', 'utc', NOW)).toBe('~15:00')
    expect(formatEstimate('2026-11-17T15:00:20Z', 'local', NOW)).toBe('~10:01')
    // Not today's: the day is named.
    expect(formatEstimate('2026-11-18T06:00:05Z', 'utc', NOW)).toBe('~Nov 18 06:01')
    expect(formatEstimate('soon', 'utc', NOW)).toBe('soon')
  })

  it("shows a tick's slot exactly: it is not an estimate", () => {
    expect(formatSlot('2026-11-17T15:00:00Z', 'utc', NOW)).toBe('15:00')
    expect(formatSlot('2026-11-18T06:00:00Z', 'utc', NOW)).toBe('Nov 18 06:00')
  })

  it('rounds an estimated length up to a unit it can stand behind', () => {
    expect([1, 5, 38, 40, 59].map(formatAbout)).toEqual(['<5s', '~5s', '~40s', '~40s', '~60s'])
    expect([60, 61, 430, 3599].map(formatAbout)).toEqual(['~1m', '~2m', '~8m', '~60m'])
    expect([3600, 5400, 90000].map(formatAbout)).toEqual(['~1h', '~2h', '~2d'])
  })
})

describe('when a node is fresh', () => {
  it('says fresh only of an estimate', () => {
    const words = nodeEstimate(node({ cause: 'catching_up', expected_fresh_at: '2026-11-17T15:00:20Z', recheck_at: '2026-11-17T15:00:00Z' }), 'utc', NOW)
    expect([words.kind, words.text]).toEqual(['fresh', 'fresh ~15:01'])
  })

  it('says rechecked where the server promises no fresh time, and why in the tooltip', () => {
    const words = nodeEstimate(node({ cause: 'undecided', recheck_at: '2026-11-17T15:00:00Z', no_estimate: 'undecided' }), 'utc', NOW)
    expect([words.kind, words.text]).toEqual(['rechecked', 'rechecked 15:00'])
    expect(words.title).toContain('no estimate: undecided until a run reports')
    expect(words.text).not.toContain('fresh')
  })

  it('says why a node has no estimate instead of a blank, in the words the server gives each reason', () => {
    const why = (no_estimate: NonNullable<Parameters<typeof nodeEstimate>[0]['no_estimate']>, cause: NonNullable<Parameters<typeof nodeEstimate>[0]['cause']>) => nodeEstimate(node({ cause, no_estimate }), 'utc', NOW).text
    expect(why('no_path', 'no_refresh_path')).toBe('no estimate')
    expect(why('paused', 'no_refresh_path')).toBe('no estimate: paused')
    expect(why('scheduler_off', 'no_refresh_path')).toBe('no estimate: no scheduler is firing its schedule')
    expect(why('failing', 'failing')).toBe('no estimate: until the failure is fixed')
    expect(why('missed', 'overdue')).toBe('no estimate: ticks missed it')
    expect(why('no_scheduler_here', 'catching_up')).toBe('no estimate: no scheduler runs here')
  })

  it('says the reason, not the recheck, where a tick cannot clear it', () => {
    const failing = nodeEstimate(node({ cause: 'failing', no_estimate: 'failing', recheck_at: '2026-11-17T15:00:00Z' }), 'utc', NOW)
    expect([failing.kind, failing.text]).toEqual(['none', 'no estimate: until the failure is fixed'])
    expect(failing.title).toContain('rechecked 15:00')
  })

  it('says when a fresh node is next rechecked, and says fresh when no tick reaches it', () => {
    expect(nodeEstimate(node({ recheck_at: '2026-11-17T15:00:00Z' }), 'utc', NOW)).toMatchObject({ kind: 'ok', text: 'rechecked 15:00' })
    expect(nodeEstimate(node(), 'utc', NOW)).toMatchObject({ kind: 'ok', text: 'fresh' })
  })
})

describe('the estimates of the generated project', () => {
  const words = (name: string) => nodeEstimate(row(name), 'utc', NOW)

  it('says fresh, with a time, only where the server promises one', () => {
    // A queued run covers it: expected fresh 10s from now, said to the minute.
    expect(row('orders_enriched')).toMatchObject({ cause: 'queued', expected_fresh_at: '2026-11-17T14:57:10Z', no_estimate: null })
    expect(words('orders_enriched')).toMatchObject({ kind: 'fresh', text: 'fresh ~14:58' })
    // Below it, a tick will look at `revenue_hourly`, and the server promises nothing of it.
    expect(row('revenue_hourly')).toMatchObject({ cause: 'queued', expected_fresh_at: null, recheck_at: '2026-11-17T15:00:00Z', no_estimate: 'not_promised' })
    expect(words('revenue_hourly')).toMatchObject({ kind: 'rechecked', text: 'rechecked 15:00' })
    expect(words('revenue_hourly').title).toContain('no estimate: a result above it may come out unchanged')
  })

  it('says why not, for each reason the server gives', () => {
    expect(['invoices', 'invoice_totals', 'ledger', 'sales_by_day', 'sales_report', 'check__invoices'].map((name) => words(name).text)).toEqual(Array(6).fill('no estimate: until the failure is fixed'))
    expect(words('forecast').text).toBe('no estimate')
    expect(words('nightly_export').text).toBe('no estimate: no scheduler is firing its schedule')
  })

  it('says when a fresh node is next looked at, and nothing where no tick reaches it', () => {
    expect(['orders_feed', 'orders', 'publish_dashboard'].map((name) => words(name))).toEqual(Array(3).fill(expect.objectContaining({ kind: 'ok', text: 'rechecked 15:00' })))
    expect(['customers', 'fx_rates'].map((name) => words(name))).toEqual(Array(2).fill(expect.objectContaining({ kind: 'ok', text: 'fresh' })))
  })

  it('reads a scope off its entry', () => {
    const scope = (id: string) => scopeEstimate(entry(id), 'utc', NOW).text
    expect(scope('root')).toBe('9 with no estimate · the rest fresh by ~14:58')
    expect(scope('file:orders.py')).toBe('4 with no estimate · the rest fresh by ~14:58')
    expect(scope('file:billing.py')).toBe('4 with no estimate')
    expect(scope('group:orders.py:reporting')).toBe('1 with no estimate')
    expect(scope('orders.py:orders')).toBe('fresh')
  })
})

describe('when a scope is fresh', () => {
  const scope = (over: Partial<Parameters<typeof scopeEstimate>[0]>) => ({ fresh_by: null, unbounded: 0, not_fresh: 0, ...over })
  it('reads the entry: all fresh by a time, how many have no estimate, or both', () => {
    expect(scopeEstimate(scope({}), 'utc', NOW).text).toBe('fresh')
    expect(scopeEstimate(scope({ not_fresh: 60, fresh_by: '2026-11-17T15:01:10Z' }), 'utc', NOW)).toMatchObject({ kind: 'fresh', text: 'all fresh by ~15:02' })
    expect(scopeEstimate(scope({ not_fresh: 152, unbounded: 152 }), 'utc', NOW)).toMatchObject({ kind: 'none', text: '152 with no estimate' })
    expect(scopeEstimate(scope({ not_fresh: 1332, unbounded: 1152, fresh_by: '2026-11-17T15:01:10Z' }), 'utc', NOW).text).toBe('1,152 with no estimate · the rest fresh by ~15:02')
  })
})
