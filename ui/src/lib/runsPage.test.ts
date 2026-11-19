import { describe, it, expect } from 'vitest'
import { replay } from './program'
import type { ActivityHour } from './types'
import { activityBarLabel, activityHourAt, activityTicks, canLoadMore, everyRun, INITIAL_RUNS, parseRunFilters, RUNS_MAX, rerunLabel, runsCommand, runsNarrowed, runsQuery, stepName, updateRuns, windowLabel, writeRunFilters, type RunsMsg } from './runsPage'

const run = (...msgs: RunsMsg[]) => replay(updateRuns, INITIAL_RUNS, msgs)

const HOUR = { since: '2026-10-29T12:00:00Z', until: '2026-10-29T13:00:00Z' }

it('offers a cache-aware re-run after cancellation', () => {
  expect(rerunLabel('cancelled')).toBe('Re-run')
  expect(rerunLabel('failed')).toBe('Re-run failed')
})

describe('run filters in the URL', () => {
  it('round-trips, and a default leaves no parameter', () => {
    const filters = { filter: 'failed' as const, query: 'orders', window: HOUR, hideSchedule: false }
    const written = writeRunFilters(new URLSearchParams('run=r1'), filters)
    expect(written.toString()).toBe('run=r1&status=failed&q=orders&since=2026-10-29T12%3A00%3A00Z&until=2026-10-29T13%3A00%3A00Z&schedule=show')
    expect(parseRunFilters(written)).toEqual(filters)
    expect(writeRunFilters(written, { filter: 'all', query: '', window: null, hideSchedule: true }).toString()).toBe('run=r1')
  })
  it('ignores what it cannot mean, and hides the schedule\'s runs unless told', () => {
    expect(parseRunFilters(new URLSearchParams('status=x&since=soon&until=later'))).toEqual({ filter: 'all', query: '', window: null, hideSchedule: true })
    // Half a window is none.
    expect(parseRunFilters(new URLSearchParams('since=2026-10-29T12:00:00Z')).window).toBeNull()
  })
  it('takes this browser\'s last choice for the schedule\'s runs when the URL is silent', () => {
    expect(parseRunFilters(new URLSearchParams(''), false).hideSchedule).toBe(false)
    expect(parseRunFilters(new URLSearchParams('schedule=hide'), false).hideSchedule).toBe(true)
    expect(parseRunFilters(new URLSearchParams('schedule=show'), true).hideSchedule).toBe(false)
    // Shown is always written, so a link opens the view it was copied from.
    const shown = { filter: 'all' as const, query: '', window: null, hideSchedule: false }
    expect(writeRunFilters(new URLSearchParams(''), shown).toString()).toBe('schedule=show')
    expect(writeRunFilters(new URLSearchParams('schedule=show'), { ...shown, hideSchedule: true }).toString()).toBe('')
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
    expect(run({ type: 'hour_toggled', window: HOUR }).model.window).toEqual(HOUR)
    expect(run({ type: 'hour_toggled', window: HOUR }, { type: 'hour_toggled', window: HOUR }).model.window).toBeNull()
  })

  it('hides the schedule\'s runs by default, and shows them when asked', () => {
    expect(INITIAL_RUNS.hideSchedule).toBe(true)
    const shown = run({ type: 'schedule_toggled' })
    expect(shown.model.hideSchedule).toBe(false)
    // Only what changed is written: another filter changed in the same frame is not undone.
    expect(shown.effects).toEqual([{ type: 'filters', change: { hideSchedule: false } }])
    // The hidden failures: the schedule's runs, the ones that did not succeed.
    const failures = run({ type: 'schedule_failures_shown' }).model
    expect([failures.hideSchedule, failures.filter]).toEqual([false, 'failed'])
  })

  it('clears what narrows the list and leaves the schedule choice', () => {
    const cleared = run({ type: 'filter_changed', filter: 'live' }, { type: 'query_changed', value: 'orders' }, { type: 'hour_toggled', window: HOUR }, { type: 'filters_cleared' }).model
    expect(runsNarrowed(cleared)).toBe(false)
    expect(cleared.hideSchedule).toBe(true)
  })

  it('retries the request that failed', () => {
    expect(run({ type: 'retry_list' }, { type: 'retry_detail' }).effects).toEqual([{ type: 'refetch_list' }, { type: 'refetch_detail' }])
  })
})

describe('the filters are the server\'s', () => {
  it('asks GET /runs for what the model shows, by the names of barca history\'s flags', () => {
    expect(runsQuery(INITIAL_RUNS)).toEqual({ limit: 100, status: null, triggered_by: null, not_triggered_by: 'schedule', since: null, until: null, search: null })
    const model = { ...INITIAL_RUNS, filter: 'failed' as const, query: ' orders ', window: HOUR, hideSchedule: false }
    expect(runsQuery(model)).toEqual({ limit: 100, status: 'not_successful', triggered_by: null, not_triggered_by: null, since: HOUR.since, until: HOUR.until, search: 'orders' })
    expect(runsQuery({ ...INITIAL_RUNS, filter: 'live' }).status).toBe('running')
    expect(everyRun(25)).toEqual({ limit: 25, status: null, triggered_by: null, not_triggered_by: null, since: null, until: null, search: null })
  })

  it('says the same in a terminal', () => {
    expect(runsCommand(INITIAL_RUNS)).toBe('barca history --not-triggered-by schedule')
    expect(runsCommand({ ...INITIAL_RUNS, filter: 'failed', query: 'daily report', window: HOUR, hideSchedule: false }))
      .toBe('barca history --status not_successful --since 2026-10-29T12:00:00Z --until 2026-10-29T13:00:00Z --search "daily report"')
  })
})

describe('run list telemetry', () => {
  it('names a step by its short name, with its key', () => {
    expect(stepName('a/p.py:orders[at=12:00]')).toBe('orders[at=12:00]')
  })

  it('labels a bar with what the server says of its hour, and ticks the axis', () => {
    // `activity.hours` of `GET /runs`: 24 hours of UTC, counted by the server.
    const counts = { success: 0, failed: 0, error: 0, cancelled: 0, interrupted: 0, running: 0 }
    const triggers = { schedule: 0, api: 0, manual: 0, client: 0, unknown: 0 }
    const start = Date.parse('2026-10-28T13:00:00Z')
    const stamp = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z')
    const hours: ActivityHour[] = Array.from({ length: 24 }, (_, i) => ({
      at: stamp(start + i * 3_600_000), until: stamp(start + (i + 1) * 3_600_000),
      runs: 0, ok: 0, not_successful: 0, in_progress: 0, by_status: counts, by_trigger: triggers, median_seconds: null, failure: null,
    }))
    hours[23] = { ...hours[23]!, runs: 62, ok: 59, not_successful: 2, in_progress: 1, median_seconds: 1.25, failure: { text: "orders: KeyError: 'region'", runs: 2 } }
    expect(activityHourAt(hours[23]!)).toBe(Date.parse('2026-10-29T12:00:00Z'))
    expect(activityBarLabel(hours[23]!, 'utc')).toBe("12:00 UTC: 62 runs, 2 not successful, 1 in progress · median 1.3s · orders: KeyError: 'region'")
    // A failure most, not all, of the hour's unsuccessful runs share says of how many.
    expect(activityBarLabel({ ...hours[23]!, not_successful: 5, failure: { text: 'cancelled', runs: 3 } }, 'utc')).toContain('· cancelled (3 of 5)')
    expect(activityBarLabel({ ...hours[0]!, runs: 1, ok: 1 }, 'utc')).toBe('13:00 UTC: 1 run')
    expect(windowLabel({ since: hours[23]!.at, until: hours[23]!.until }, 'utc')).toBe('12:00–13:00 UTC')
    const ticks = activityTicks(hours, 6, 'utc')
    expect(ticks.map((t) => t.label)).toEqual(['18:00', '00:00', '06:00'])
    expect(ticks.every((t) => t.index < 20)).toBe(true)
  })
})
