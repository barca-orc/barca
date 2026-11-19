import { describe, expect, it } from 'vitest'
import { affectedSchedules, fileOf, pausedBanner, pausedIds } from './pauses'
import { pauseCover } from './testdata/project'
import type { ScheduleJob } from './types'

const job = (id: string, next_fire: number | null): ScheduleJob => ({
  state: 'on_time', state_detail: null, queue_bound: false, typical_queue_wait_seconds: 0, typical_run_seconds: null,
  id, cron: '* * * * *', kind: 'asset', next_fire, last_fired: null, last_run_id: null, last_run: null, last_status: null, late: false, late_since: null,
  last_skipped: null, skipped_for: null, skip_reason: null, paused: false, paused_until: null,
  failures: 0, max_failures: 5, failing: false,
})

describe('affectedSchedules', () => {
  // What a pause of `orders` covers is the server's answer (the scheduler's walk in barca-service).
  const cover = pauseCover('orders')
  const [feed, hourly] = cover.schedules.map((s) => s.id) as [string, string]

  it('shows the served schedules and relations, each with its job, soonest next tick first', () => {
    expect(cover.schedules.map((s) => s.relation)).toEqual(['target', 'input'])
    const jobs = [job(feed, 30), job(hourly, 10), job('orders.py:nightly_export', 5)]
    expect(affectedSchedules(cover, jobs).map((s) => [s.job.id, s.relation])).toEqual([[hourly, 'input'], [feed, 'target']])
  })

  it('adds no schedule the server did not name, and waits for the jobs and the answer', () => {
    expect(affectedSchedules(cover, [job('orders.py:nightly_export', 5)])).toEqual([])
    expect(affectedSchedules(undefined, [job(feed, 1)])).toEqual([])
    // A schedule with no next tick (held, paused for good) goes last.
    expect(affectedSchedules(cover, [job(feed, null), job(hourly, 10)]).map((s) => s.job.id)).toEqual([hourly, feed])
  })
})

describe('pauses', () => {
  it('reads ids, files and the banner', () => {
    expect(pausedIds({ total: 1, paused: [{ id: 'p.py:a', paused_at: 1, via: 'a' }], schedules: [] }).has('p.py:a')).toBe(true)
    expect(pausedIds(undefined).size).toBe(0)
    expect(fileOf('dir/p.py:a')).toBe('dir/p.py')
    expect(pausedBanner(0)).toBeNull()
    expect(pausedBanner(1)).toEqual({ headline: '1 asset paused.', detail: 'Scheduled runs skip it until it is resumed.' })
    expect(pausedBanner(3)).toEqual({ headline: '3 assets paused.', detail: 'Scheduled runs skip them until they are resumed.' })
  })
})
