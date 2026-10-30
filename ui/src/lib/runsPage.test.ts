import { describe, it, expect } from 'vitest'
import { replay } from './program'
import { canLoadMore, INITIAL_RUNS, RUNS_MAX, runActivity, updateRuns, visibleRuns, type RunsMsg } from './runsPage'

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

  it('retries the request that failed', () => {
    expect(run({ type: 'retry_list' }, { type: 'retry_detail' }).effects).toEqual([{ type: 'refetch_list' }, { type: 'refetch_detail' }])
  })
})

describe('run list telemetry', () => {
  const run = (id: string, status: string, minutesAgo: number, elapsed: number | null = 2) => ({
    id, handle: null, run_id: id, command: 'get', files: [], target: `t_${id}`, status, triggered_by: null,
    scheduled_at: null, schedule_id: null, started_at: new Date(NOW - minutesAgo * 60_000).toISOString(),
    finished_at: null, elapsed_seconds: elapsed, steps_total: null, steps_executed: 0, steps_cached: 0, error: null,
  })
  const NOW = Date.parse('2026-10-29T12:30:00Z')
  const runs = [run('a', 'success', 5, 1), run('b', 'failed', 10, 3), run('c', 'running', 1, null), run('d', 'cancelled', 90, 5), run('e', 'success', 60 * 30)]

  it('filters by outcome and by target', () => {
    expect(visibleRuns(runs, { filter: 'failed', query: '' }).map((r) => r.id)).toEqual(['b', 'd'])
    expect(visibleRuns(runs, { filter: 'live', query: '' }).map((r) => r.id)).toEqual(['c'])
    expect(visibleRuns(runs, { filter: 'all', query: 't_A' }).map((r) => r.id)).toEqual(['a'])
  })

  it('counts the last day hour by hour, with the median of finished runs', () => {
    const day = runActivity(runs, NOW)
    expect(day.hours).toHaveLength(24)
    expect(day).toMatchObject({ total: 4, failed: 2, medianSeconds: 3, partial: false })
    expect(day.hours.at(-1)).toMatchObject({ ok: 1, failed: 1, live: 1 })
    expect(runActivity(runs.slice(0, 2), NOW, 24, true).partial).toBe(true)
  })
})
