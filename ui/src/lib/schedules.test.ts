import { describe, expect, it } from 'vitest'
import type { ListNode, NodeState } from './types'
import type { ScheduleStatus } from './generated/ScheduleStatus'
import { formatLate, humanizeCron, lastStatusLabel, scheduleRows } from './schedules'
import { formatCountdown } from './time'
import { served } from './testdata/served'

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

function state(id: string, over: Partial<NodeState> = {}): NodeState {
  return served({
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
  })
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
    const rows = scheduleRows(assets, states)
    expect(rows.map((r) => r.name)).toEqual(['heartbeat', 'refresh'])
  })

  it('carries the cron, its words and the next run; the last tick is the schedule\'s, not the node\'s last result', () => {
    const [, refresh] = scheduleRows(assets, states)
    expect(refresh).toMatchObject({
      cron: '*/10 * * * *',
      human: 'every 10 minutes',
      nextRunMs: now + 300_000,
      state: 'always_runs',
      lastFailed: true,
      // No tick recorded: the node's own last materialization is not its schedule's last run.
      last: null,
    })
  })

  it('a scheduled node the server has no state for still shows', () => {
    const rows = scheduleRows([asset('job.py:new', '0 6 * * *')], [])
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ name: 'new', nextRunMs: null, last: null })
  })

})

describe('lateness', () => {
  const now = Date.parse('2026-11-16T12:21:30Z')
  const s = (iso: string) => Date.parse(iso) / 1000
  const stands = (over: Partial<ScheduleStatus> = {}): ScheduleStatus => ({
    cron: '* * * * *', next_fire: null, last_fired: null, last_status: null, last_run_id: null,
    state: 'on_time', state_detail: null, late: false, late_since: null, failures: 0, max_failures: 5, failing: false, paused: false,
    paused_until: null, skip_reason: null, live: true, typical_queue_wait_seconds: 0,
    typical_run_seconds: null, queue_bound: false, ...over,
  })

  it('shows a held schedule without a next-run countdown', () => {
    const id = 'job.py:pulse'
    const [row] = scheduleRows([asset(id, '* * * * *')], [
      state(id, { next_run: now / 1000 + 10, schedule: stands({ state: 'held', state_detail: 'held after 3 failed ticks' }) }),
    ])
    expect(row).toMatchObject({ standing: 'held', nextRunMs: null, detail: 'held after 3 failed ticks' })
  })

  it('words how long ago the missed slot was', () => {
    expect(formatLate(now - 2 * 3_600_000 - 5 * 60_000, now)).toBe('late by 2h 5m')
    expect(formatLate(now - 30_000, now)).toBe('late by 30s')
  })

  it('a row is late when the server says so, and only then', () => {
    const assets = [asset('job.py:minutely', '* * * * *'), asset('job.py:queued', '* * * * *'), asset('job.py:soon', '*/5 * * * *')]
    const rows = scheduleRows(
      assets,
      [
        state('job.py:soon', { next_run: s('2026-11-16T12:25:00Z'), schedule: stands() }),
        // Slots have passed since its last tick, and the server does not call it late: its
        // run waits in the run queue. Nothing here second-guesses that from the times.
        state('job.py:queued', {
          next_run: s('2026-11-16T12:22:00Z'),
          late_since: s('2026-11-16T12:01:00Z'),
          schedule: stands({ late: false, late_since: '2026-11-16T12:01:00Z', last_fired: '2026-11-16T12:00:00Z', last_status: 'pending', last_run_id: 'r7' }),
        }),
        state('job.py:minutely', {
          next_run: s('2026-11-16T12:22:00Z'),
          schedule: stands({ late: true, late_since: '2026-11-16T12:09:00Z', last_fired: '2026-11-16T12:08:00Z', last_status: 'success', last_run_id: 'r3' }),
        }),
      ],
      now,
    )
    // Late first, then soonest next run.
    expect(rows.map((r) => [r.name, r.late?.by ?? null])).toEqual([
      ['minutely', 'late by 12m'],
      ['queued', null],
      ['soon', null],
    ])
    expect(rows[0]?.last).toEqual({ status: 'success', at: '2026-11-16T12:08:00Z', runId: 'r3' })
    expect(rows[1]?.last).toEqual({ status: 'pending', at: '2026-11-16T12:00:00Z', runId: 'r7' })
    expect(lastStatusLabel('pending')).toBe('queued')
    expect(lastStatusLabel('failed')).toBe('failed')
    expect(rows[2]?.last).toBeNull()
  })

  it("puts late then queue-bound schedules first, and says the typical wait and run as estimates", () => {
    const rows = scheduleRows(
      [asset('job.py:soon', '*/5 * * * *'), asset('job.py:pulse', '*/10 * * * * *'), asset('job.py:minutely', '* * * * *')],
      [
        state('job.py:soon', { next_run: s('2026-11-16T12:22:00Z'), schedule: stands({ typical_run_seconds: 4 }) }),
        // The server's words: its ticks wait longer than they are apart.
        state('job.py:pulse', { next_run: s('2026-11-16T12:25:00Z'), schedule: stands({ queue_bound: true, typical_queue_wait_seconds: 12, typical_run_seconds: 1 }) }),
        state('job.py:minutely', { next_run: s('2026-11-16T12:30:00Z'), schedule: stands({ late: true, late_since: '2026-11-16T12:09:00Z' }) }),
      ],
      now,
    )
    expect(rows.map((r) => [r.name, r.late !== null, r.queueBound, r.queueWait, r.typicalRun])).toEqual([
      ['minutely', true, false, null, null],
      ['pulse', false, true, '~15s', '<5s'],
      ['soon', false, false, null, '<5s'],
    ])
    // A row without a schedule block is neither: nothing is worked out from its times.
    expect(scheduleRows([asset('job.py:new', '0 6 * * *')], [])[0]).toMatchObject({ queueBound: false, queueWait: null, typicalRun: null })
  })

  it('reads a pause from the schedule block, with when a timed pause ends', () => {
    const rows = scheduleRows(
      [asset('job.py:held', '* * * * *'), asset('job.py:open', '* * * * *'), asset('job.py:runs', '* * * * *'), asset('job.py:new', '* * * * *')],
      [
        state('job.py:held', { schedule: stands({ paused: true, paused_until: '2026-11-16T13:00:00Z' }) }),
        state('job.py:open', { schedule: stands({ paused: true }) }),
        state('job.py:runs', { schedule: stands() }),
      ],
      now,
    )
    const paused = Object.fromEntries(rows.map((r) => [r.name, r.paused]))
    // A node `GET /state` has no row for yet is not shown as paused.
    expect(paused).toEqual({ held: { until: Date.parse('2026-11-16T13:00:00Z') }, open: { until: null }, runs: null, new: null })
  })
})
