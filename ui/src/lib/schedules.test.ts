import { describe, expect, it } from 'vitest'
import type { ListNode, NodeState, ScheduleJob } from './types'
import { formatCountdown, formatLate, humanizeCron, lateGraceMs, lateness, scheduleRows } from './schedules'

describe('humanizeCron', () => {
  it('describes common schedules in words', () => {
    expect(humanizeCron('*/10 * * * *')).toBe('every 10 minutes')
    expect(humanizeCron('* * * * *')).toBe('every minute')
    expect(humanizeCron('*/15 * * * * *')).toBe('every 15 seconds')
    expect(humanizeCron('0 * * * *')).toBe('hourly at :00')
    expect(humanizeCron('30 6 * * *')).toBe('daily at 06:30')
    expect(humanizeCron('0 9 * * 1')).toBe('weekly on Monday at 09:00')
    expect(humanizeCron('0 0 1 * *')).toBe('monthly on day 1 at 00:00')
  })
  it('anything else stays as the raw expression', () => {
    expect(humanizeCron('0 9-17 * * 1-5')).toBeNull()
    expect(humanizeCron('not a cron')).toBeNull()
  })
})

describe('formatCountdown', () => {
  const now = 1_000_000_000_000
  it('scales its unit to the distance', () => {
    expect(formatCountdown(now + 12_000, now)).toBe('in 12s')
    expect(formatCountdown(now + 12 * 60_000, now)).toBe('in 12m')
    expect(formatCountdown(now + (3 * 60 + 5) * 60_000, now)).toBe('in 3h 5m')
    expect(formatCountdown(now + 2 * 86_400_000 + 5_000, now)).toBe('in 2d')
  })
  it('a time already past is due now', () => {
    expect(formatCountdown(now - 1, now)).toBe('due now')
  })
})

function asset(id: string, cron: string | null, kind: ListNode['kind'] = 'task'): ListNode {
  return {
    id,
    kind,
    ...(cron ? { freshness: 'schedule' as const, schedule: cron } : { freshness: 'always' as const }),
    inputs: [],
    env: [], file: 'p.py', line: 1,
  }
}

function job(id: string, over: Partial<ScheduleJob> = {}): ScheduleJob {
  return {
    id,
    cron: '*/10 * * * *',
    kind: 'task',
    next_fire: null,
    last_fired: null,
    last_run: null,
    last_status: null,
    last_skipped: null,
    skipped_for: null,
    skip_reason: null,
    paused: false,
    paused_until: null,
    failures: 0,
    max_failures: 5,
    failing: false,
    ...over,
  }
}

function state(id: string, over: Partial<NodeState> = {}): NodeState {
  return {
    id,
    name: id.split(':').pop()!,
    kind: 'task',
    inputs: [],
    partitioned: false,
    cache: { state: 'always_runs', reason: 'task', detail: '' },
    last_materialization: null,
    shape: null,
    env: [],
    durations: null,
    next_run: null,
    prev_run: null,
    late_since: null,
    signals: [],
    last_success_at: null, failing_since: null, consecutive_failures: 0,
    running: null,
    ...over,
  }
}

describe('scheduleRows', () => {
  const now = Date.parse('2026-10-06T12:00:00Z')
  const assets = [
    asset('job.py:refresh', '*/10 * * * *'),
    asset('job.py:heartbeat', '*/15 * * * * *'),
    asset('job.py:manual_thing', null, 'asset'),
  ]
  const states = [
    state('job.py:refresh', {
      next_run: now / 1000 + 300,
      last_materialization: {
        status: 'failed',
        created_at: '2026-10-06 11:50:00',
        elapsed_seconds: 0.1,
        run_hash: null,
        artifact: null,
        format: null,
        size_bytes: null,
        rows: null,
        error: 'boom',
      },
    }),
    state('job.py:heartbeat', { next_run: now / 1000 + 10 }),
    state('job.py:manual_thing', { kind: 'asset' }),
  ]

  it('only scheduled nodes, soonest first', () => {
    const rows = scheduleRows(assets, states, now)
    expect(rows.map((r) => r.name)).toEqual(['heartbeat', 'refresh'])
  })

  it('carries the cron, its words, the countdown and the last run', () => {
    const [, refresh] = scheduleRows(assets, states, now)
    expect(refresh).toMatchObject({
      cron: '*/10 * * * *',
      human: 'every 10 minutes',
      nextIn: 'in 5m',
      state: 'always_runs',
      lastFailed: true,
      last: { status: 'failed', ago: '10m ago' },
    })
  })

  it('a scheduled node the server has no state for still shows', () => {
    const rows = scheduleRows([asset('job.py:new', '0 6 * * *')], [], now)
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ name: 'new', nextRunMs: null, nextIn: null, last: null })
  })

  it('is not paused without a scheduler entry or when the job runs', () => {
    expect(scheduleRows(assets, states, now).every((r) => r.paused === null)).toBe(true)
    const rows = scheduleRows(assets, states, now, [job('job.py:refresh')])
    expect(rows.find((r) => r.name === 'refresh')?.paused).toBeNull()
  })

  it('shows a paused job, with when a timed pause ends', () => {
    const rows = scheduleRows(assets, states, now, [
      job('job.py:refresh', { paused: true, paused_until: now / 1000 + 3600, skip_reason: 'paused' }),
      job('job.py:heartbeat', { paused: true }),
    ])
    expect(rows.find((r) => r.name === 'refresh')?.paused).toEqual({ until: now + 3_600_000 })
    expect(rows.find((r) => r.name === 'heartbeat')?.paused).toEqual({ until: null })
  })

  it("falls back to the scheduler's next fire when /state has none", () => {
    const rows = scheduleRows([asset('job.py:new', '0 6 * * *')], [], now, [
      job('job.py:new', { next_fire: now / 1000 + 60 }),
    ])
    expect(rows[0]).toMatchObject({ nextRunMs: now + 60_000, nextIn: 'in 1m' })
  })
})

describe('lateness', () => {
  const now = Date.parse('2026-11-16T12:21:30Z')
  const s = (iso: string) => Date.parse(iso) / 1000
  // Every 5 minutes: slots 12:20 and 12:25 around now.
  const slots = { prev_run: s('2026-11-16T12:20:00Z'), next_run: s('2026-11-16T12:25:00Z') }

  it('is late once the first missed slot is further back than the grace', () => {
    expect(lateness({ ...slots, late_since: s('2026-11-16T12:05:00Z') }, now, false)).toEqual({
      sinceMs: Date.parse('2026-11-16T12:05:00Z'),
      by: 'late by 16m',
    })
    // The 12:20 slot has had 90 s: a minute's grace has passed.
    expect(lateness({ ...slots, late_since: slots.prev_run }, now, false)?.by).toBe('late by 1m')
  })

  it('is on time with no missed slot, within the grace, or while paused', () => {
    expect(lateness({ ...slots, late_since: null }, now, false)).toBeNull()
    expect(lateness({ ...slots, late_since: slots.prev_run }, Date.parse('2026-11-16T12:20:40Z'), false)).toBeNull()
    expect(lateness({ ...slots, late_since: s('2026-11-16T12:05:00Z') }, now, true)).toBeNull()
    expect(lateness(undefined, now, false)).toBeNull()
  })

  it('gives a frequent schedule half its interval, at most a minute', () => {
    expect(lateGraceMs(0, 15_000)).toBe(7_500)
    expect(lateGraceMs(0, 3_600_000)).toBe(60_000)
    expect(lateGraceMs(null, null)).toBe(60_000)
    expect(formatLate(now - 2 * 3_600_000 - 5 * 60_000, now)).toBe('late by 2h 5m')
    expect(formatLate(now - 30_000, now)).toBe('late by 30s')
  })

  it('a late row says so instead of a countdown, and sorts first', () => {
    const assets = [asset('job.py:minutely', '* * * * *'), asset('job.py:soon', '*/5 * * * *')]
    const rows = scheduleRows(
      assets,
      [
        state('job.py:minutely', {
          prev_run: s('2026-11-16T12:21:00Z'),
          next_run: s('2026-11-16T12:22:00Z'),
          late_since: s('2026-11-16T12:09:00Z'),
        }),
        state('job.py:soon', { ...slots, late_since: null }),
      ],
      now,
    )
    expect(rows.map((r) => [r.name, r.nextIn, r.late?.by ?? null])).toEqual([
      ['minutely', null, 'late by 12m'],
      ['soon', 'in 3m', null],
    ])
  })
})
