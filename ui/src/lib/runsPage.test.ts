import { describe, it, expect } from 'vitest'
import { replay } from './program'
import type { ActivityHour, RunSummary } from './types'
import { activityBarLabel, activityHourAt, activityTicks, canLoadMore, failedChipCount, INITIAL_RUNS, runsCountLabel, parseRunFilters, RUNS_MAX, runsTriggeredBy, stepName, updateRuns, visibleRuns, writeRunFilters, type RunsMsg } from './runsPage'

const run = (...msgs: RunsMsg[]) => replay(updateRuns, INITIAL_RUNS, msgs)

describe('run filters in the URL', () => {
  it('round-trips, and a default leaves no parameter', () => {
    const filters = { filter: 'failed' as const, query: 'orders', hour: 3_600_000, hideSchedule: true }
    const written = writeRunFilters(new URLSearchParams('run=r1'), filters)
    expect(written.toString()).toBe('run=r1&status=failed&q=orders&hour=3600000&schedule=hide')
    expect(parseRunFilters(written)).toEqual(filters)
    expect(writeRunFilters(written, { filter: 'all', query: '', hour: null, hideSchedule: false }).toString()).toBe('run=r1')
  })
  it('ignores what it cannot mean', () => {
    expect(parseRunFilters(new URLSearchParams('status=x&hour=soon'))).toEqual({ filter: 'all', query: '', hour: null, hideSchedule: false })
  })
})

describe('runs program', () => {
  it('selects and closes through the URL, replacing a resolved handle', () => {
    expect(run({ type: 'run_selected', id: 'r1' }, { type: 'run_resolved', id: '42' }, { type: 'panel_closed' }).effects).toEqual([
      { type: 'navigate', run: 'r1', replace: false },
      { type: 'navigate', run: '42', replace: true },
      { type: 'navigate', run: null, replace: false },
    ])
  })

  it('loads more runs a page at a time, up to the cap', () => {
    expect(run({ type: 'load_more' }).model.limit).toBe(200)
    const capped = run(...Array.from({ length: 20 }, (): RunsMsg => ({ type: 'load_more' }))).model
    expect(capped.limit).toBe(RUNS_MAX)
    expect(canLoadMore(capped)).toBe(false)
  })

  it('toggles the hour filter', () => {
    expect(run({ type: 'hour_toggled', at: 3600 }).model.hour).toBe(3600)
    expect(run({ type: 'hour_toggled', at: 3600 }, { type: 'hour_toggled', at: 3600 }).model.hour).toBeNull()
  })

  it('hides the schedule\'s runs by asking the server for the other triggers', () => {
    const hidden = run({ type: 'schedule_toggled' })
    expect(hidden.model.hideSchedule).toBe(true)
    expect(hidden.effects).toEqual([{ type: 'filters', filters: { filter: 'all', query: '', hour: null, hideSchedule: true } }])
    expect(runsTriggeredBy(hidden.model)).toBe('manual,api,client')
    expect(runsTriggeredBy(run({ type: 'schedule_toggled' }, { type: 'schedule_toggled' }).model)).toBeNull()
  })

  it('retries the request that failed', () => {
    expect(run({ type: 'retry_list' }, { type: 'retry_detail' }).effects).toEqual([{ type: 'refetch_list' }, { type: 'refetch_detail' }])
  })
})

describe('run list telemetry', () => {
  const run = (id: string, status: string, minutesAgo: number, elapsed: number | null = 2) => ({
    id, handle: null, run_id: id, command: 'get', files: [], target: `t_${id}`, status, triggered_by: null,
    scheduled_at: null, schedule_id: null, started_at: new Date(NOW - minutesAgo * 60_000).toISOString(),
    finished_at: null, elapsed_seconds: elapsed, steps_total: null, steps_executed: 0, steps_cached: 0, error: null, first_failure: null,
    principal: null, trigger_reason: null, cancellation: null,
    line: { steps: '0 ok · 0 cached', steps_detail: '', failure: null, failure_detail: null },
  })
  const NOW = Date.parse('2026-10-29T12:30:00Z')
  const runs = [run('a', 'success', 5, 1), run('b', 'failed', 10, 3), run('c', 'running', 1, null), run('d', 'cancelled', 90, 5), run('e', 'success', 60 * 30)]

  it('filters by outcome and by target', () => {
    expect(visibleRuns(runs, { filter: 'failed', query: '', hour: null }).map((r) => r.id)).toEqual(['b', 'd'])
    expect(visibleRuns(runs, { filter: 'live', query: '', hour: null }).map((r) => r.id)).toEqual(['c'])
    expect(visibleRuns(runs, { filter: 'all', query: 't_A', hour: null }).map((r) => r.id)).toEqual(['a'])
  })

  it('filters to one hour of the strip', () => {
    const hour = Date.parse('2026-10-29T12:00:00Z')
    expect(visibleRuns(runs, { filter: 'all', query: '', hour }).map((r) => r.id)).toEqual(['a', 'b', 'c'])
    expect(visibleRuns(runs, { filter: 'all', query: '', hour: hour - 3_600_000 }).map((r) => r.id)).toEqual(['d'])
  })

  it('names a step by its short name, with its key', () => {
    expect(stepName('a/p.py:orders[at=12:00]')).toBe('orders[at=12:00]')
  })

  it('labels a bar with the server\'s counts for its hour, and ticks the axis', () => {
    // `activity.hours` of `GET /runs`: 24 hours of UTC, counted by the server.
    const counts = { success: 0, failed: 0, error: 0, cancelled: 0, interrupted: 0, running: 0 }
    const triggers = { schedule: 0, api: 0, manual: 0, client: 0, unknown: 0 }
    const start = Date.parse('2026-10-28T13:00:00Z')
    const hours: ActivityHour[] = Array.from({ length: 24 }, (_, i) => ({
      at: new Date(start + i * 3_600_000).toISOString().replace('.000Z', 'Z'),
      runs: 0, ok: 0, not_successful: 0, in_progress: 0, by_status: counts, by_trigger: triggers,
    }))
    hours[23] = { ...hours[23]!, runs: 62, ok: 59, not_successful: 2, in_progress: 1 }
    expect(activityHourAt(hours[23]!)).toBe(Date.parse('2026-10-29T12:00:00Z'))
    expect(activityBarLabel(hours[23]!, 'utc')).toBe('12:00 UTC: 62 runs, 2 not successful, 1 in progress')
    expect(activityBarLabel({ ...hours[0]!, runs: 1, ok: 1 }, 'utc')).toBe('13:00 UTC: 1 run')
    const ticks = activityTicks(hours, 6, 'utc')
    expect(ticks.map((t) => t.label)).toEqual(['18:00', '00:00', '06:00'])
    expect(ticks.every((t) => t.index < 20)).toBe(true)
  })
})

describe('the header count', () => {
  const r = (status: string) => ({ status }) as RunSummary
  const loaded = [...Array.from({ length: 21 }, () => r('failed')), ...Array.from({ length: 79 }, () => r('success'))]
  const partial = { runs: loaded, total: 37_955, truncated: true }
  const all = { filter: 'all' as const, query: '', hour: null }
  it('is every run the server has while nothing narrows the list', () => {
    expect(runsCountLabel(partial, 100, all).text).toBe('37,955')
  })
  it('says a count over the latest runs loaded is partial', () => {
    expect(runsCountLabel(partial, 21, { ...all, filter: 'failed' }).text).toBe('21 not successful in the latest 100')
    expect(runsCountLabel(partial, 1, { ...all, filter: 'live' }).text).toBe('1 in progress in the latest 100')
    expect(runsCountLabel(partial, 3, { ...all, query: 'orders' }).text).toBe('3 matching in the latest 100')
    expect(runsCountLabel(partial, 21, { ...all, filter: 'failed' }).title).toContain('Load more')
  })
  it('is a share of all runs when all of them are loaded', () => {
    expect(runsCountLabel({ runs: loaded, total: 100, truncated: false }, 21, { ...all, filter: 'failed' }).text).toBe('21 of 100')
  })
  it('the Not successful chip says what it counted over when the list is partial', () => {
    expect(failedChipCount(partial)).toEqual({ count: 21, title: '21 not successful in the latest 100 runs loaded' })
    expect(failedChipCount({ runs: loaded, truncated: false })).toEqual({ count: 21, title: undefined })
  })
})
