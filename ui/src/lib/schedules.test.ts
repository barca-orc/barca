import { describe, expect, it } from 'vitest'
import type { AssetSummary, NodeState } from './types'
import { formatCountdown, humanizeCron, scheduleRows } from './schedules'

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

function asset(id: string, cron: string | null, kind: AssetSummary['kind'] = 'task'): AssetSummary {
  return {
    id,
    kind,
    freshness: cron ? { type: 'Schedule', value: cron } : { type: 'Always' },
    inputs: [],
    env: [],
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
      severity: 'failed',
      last: { status: 'failed', ago: '10m ago' },
    })
  })

  it('a scheduled node the server has no state for still shows', () => {
    const rows = scheduleRows([asset('job.py:new', '0 6 * * *')], [], now)
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ name: 'new', nextRunMs: null, nextIn: null, last: null })
  })
})
