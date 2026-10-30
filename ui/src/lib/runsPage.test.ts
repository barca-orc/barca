import { describe, it, expect } from 'vitest'
import { replay } from './program'
import type { RunSummary } from './types'
import { activityBarLabel, activityTicks, canLoadMore, INITIAL_RUNS, RUNS_MAX, runActivity, runFailureLine, runProgress, runRowError, stepName, updateRuns, visibleRuns, type RunsMsg } from './runsPage'

const run = (...msgs: RunsMsg[]) => replay(updateRuns, INITIAL_RUNS, msgs)

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

  it('retries the request that failed', () => {
    expect(run({ type: 'retry_list' }, { type: 'retry_detail' }).effects).toEqual([{ type: 'refetch_list' }, { type: 'refetch_detail' }])
  })
})

describe('run list telemetry', () => {
  const run = (id: string, status: string, minutesAgo: number, elapsed: number | null = 2) => ({
    id, handle: null, run_id: id, command: 'get', files: [], target: `t_${id}`, status, triggered_by: null,
    scheduled_at: null, schedule_id: null, started_at: new Date(NOW - minutesAgo * 60_000).toISOString(),
    finished_at: null, elapsed_seconds: elapsed, steps_total: null, steps_executed: 0, steps_cached: 0, error: null, first_failure: null,
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

  it('counts the last day hour by hour, with the median of finished runs', () => {
    const day = runActivity(runs, NOW)
    expect(day.hours).toHaveLength(24)
    expect(day).toMatchObject({ total: 4, failed: 2, medianSeconds: 3, partial: false })
    expect(day.hours.at(-1)).toMatchObject({ ok: 1, failed: 1, live: 1 })
    expect(runActivity(runs.slice(0, 2), NOW, 24, true).partial).toBe(true)
  })

  const failure = { node_id: 'p.py:orders[day=1]', error: 'KeyError: x' }

  it('names the first failed step when the run has no error of its own', () => {
    expect(stepName('a/p.py:orders[at=12:00]')).toBe('orders[at=12:00]')
    expect(runRowError({ ...run('f', 'failed', 1), first_failure: failure })?.text).toBe('orders[day=1]: KeyError: x')
    expect(runRowError({ ...run('f', 'failed', 1), error: 'boom\ntrace', first_failure: failure })?.text).toBe('boom')
    expect(runRowError(run('f', 'failed', 1))?.text).toBe('No failed step recorded: open the run')
    expect(runRowError(run('s', 'success', 1))).toBeNull()
    expect(runRowError(run('c', 'cancelled', 1))).toBeNull()
  })

  it('shows progress and where the run stopped', () => {
    const stopped = { ...run('f', 'failed', 1), steps_total: 28404, steps_executed: 21950, first_failure: failure }
    expect(runProgress(stopped)).toMatchObject({ steps: `${(21950).toLocaleString()} / ${(28404).toLocaleString()}`, stoppedAt: 'orders[day=1]' })
    expect(runProgress({ ...run('s', 'success', 1), steps_executed: 2, steps_cached: 1 })).toMatchObject({ steps: '3', stoppedAt: null })
  })

  it('labels a bar with what failed first, and ticks the axis', () => {
    const day = runActivity([{ ...run('f', 'failed', 10), first_failure: failure }], NOW)
    expect(activityBarLabel(day.hours.at(-1)!)).toContain('1 not successful\norders[day=1]: KeyError: x')
    const ticks = activityTicks(day.hours)
    expect(ticks.length).toBeGreaterThanOrEqual(3)
    expect(ticks.every((t) => t.index < 22)).toBe(true)
  })
})

describe('runFailureLine', () => {
  const run = { status: 'failed', error: null, first_failure: { node_id: 'p.py:orders', error: 'KeyError: x\ntrace' } } as unknown as RunSummary
  it('says where a run stopped and why', () => expect(runFailureLine(run)).toBe('stopped at orders · KeyError: x'))
  it('is nothing for a run with nothing to say', () => {
    expect(runFailureLine({ ...run, status: 'success', first_failure: null })).toBeNull()
  })
})
