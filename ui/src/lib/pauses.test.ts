import { describe, expect, it } from 'vitest'
import { blastRadius, fileOf, pausedBanner, pausedIds } from './pauses'
import type { ScheduleJob } from './types'

const graph = [
  { id: 'p.py:raw', inputs: [] },
  { id: 'p.py:clean', inputs: ['p.py:raw'] },
  { id: 'p.py:report', inputs: ['p.py:clean'] },
  { id: 'p.py:other', inputs: [] },
]

const job = (id: string, next_fire: number | null): ScheduleJob => ({
  id, cron: '* * * * *', kind: 'asset', next_fire, last_fired: null, last_run: null, last_status: null,
  last_skipped: null, skipped_for: null, skip_reason: null, paused: false, paused_until: null,
  failures: 0, max_failures: 5, failing: false,
})

describe('blastRadius', () => {
  it('lists every downstream node, transitively', () => {
    expect(blastRadius('p.py:raw', graph, []).downstream).toEqual(['p.py:clean', 'p.py:report'])
    expect(blastRadius('p.py:report', graph, []).downstream).toEqual([])
  })

  it('finds the schedules that compute it, read it or reconcile it, soonest first', () => {
    const jobs = [job('p.py:report', 30), job('p.py:raw', 10), job('p.py:other', 5), job('p.py:clean', null)]
    const { schedules } = blastRadius('p.py:clean', graph, jobs)
    expect(schedules.map((s) => [s.job.id, s.relation])).toEqual([
      ['p.py:raw', 'upstream'],
      ['p.py:report', 'downstream'],
      ['p.py:clean', 'self'],
    ])
  })
})

describe('pauses', () => {
  it('reads ids, files and the banner', () => {
    expect(pausedIds({ total: 1, paused: [{ id: 'p.py:a', paused_at: 1, via: 'a' }] }).has('p.py:a')).toBe(true)
    expect(pausedIds(undefined).size).toBe(0)
    expect(fileOf('dir/p.py:a')).toBe('dir/p.py')
    expect(pausedBanner(0)).toBeNull()
    expect(pausedBanner(1)).toEqual({ headline: '1 asset paused.', detail: 'Scheduled runs skip it until it is resumed.' })
    expect(pausedBanner(3)).toEqual({ headline: '3 assets paused.', detail: 'Scheduled runs skip them until they are resumed.' })
  })
})
