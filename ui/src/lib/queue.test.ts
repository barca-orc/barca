import { describe, expect, it } from 'vitest'
import type { QueueEntry, QueueView } from './types'
import {
  queueMatters,
  QUEUE_POLL_BUSY_MS,
  QUEUE_POLL_IDLE_MS,
  capacityLabel,
  healthQueueLabel,
  queueControl,
  queuePollInterval,
  queueRows,
  queueState,
  queueStateLabel,
  queuedLabel,
  sharedCapacityLabel,
  waitingRows,
} from './queue'

function entry(run_id: string, over: Partial<QueueEntry> = {}): QueueEntry {
  return {
    run_id,
    status: 'pending',
    position: 1,
    command: 'get',
    target: null,
    triggered_by: 'api',
    schedule_id: null,
    queued_at: '2026-10-22T10:00:00Z',
    ...over,
  }
}

function view(over: Partial<QueueView> = {}): QueueView {
  return { paused: false, paused_at: null, draining: false, max_runs: 4, running: [], queued: [], capacity: null, ...over }
}

describe('shared capacity', () => {
  const capacity = {
    total: 4,
    source: '--capacity',
    in_use: 3,
    runs: [{ run_id: 'r1', weight: 3 }],
    waiting: [{ run_id: 'r2', node: 'p.py:big', weight: 3, waiting_seconds: 12.4 }],
  }
  it('says nothing without one', () => {
    expect(sharedCapacityLabel(null)).toBeNull()
    expect(waitingRows(null)).toEqual([])
  })
  it('shows the weight in use and the steps waiting, in order', () => {
    expect(sharedCapacityLabel(capacity)).toBe('weight 3 of 4 in use · 1 step waiting')
    expect(sharedCapacityLabel({ ...capacity, waiting: [] })).toBe('weight 3 of 4 in use')
    expect(waitingRows(capacity)).toEqual([
      { position: '#1', node: 'p.py:big', runId: 'r2', weight: 3, waited: '12s', href: expect.stringContaining('r2') },
    ])
  })
})

const iso = (value: string | null) => value ?? '–'

describe('queueState', () => {
  it('is open, paused with its time, or draining', () => {
    expect(queueState(view())).toEqual({ kind: 'open' })
    expect(queueState(view({ paused: true, paused_at: '2026-10-22T09:00:00Z' })))
      .toEqual({ kind: 'paused', since: '2026-10-22T09:00:00Z' })
  })
  it('draining wins over paused', () => {
    expect(queueState(view({ paused: true, draining: true }))).toEqual({ kind: 'draining' })
  })
})

describe('queueStateLabel', () => {
  it('says nothing while open', () => {
    expect(queueStateLabel({ kind: 'open' }, iso)).toBeNull()
  })
  it('names when it was paused', () => {
    expect(queueStateLabel({ kind: 'paused', since: '2026-10-22T09:00:00Z' }, iso)).toBe('paused since 2026-10-22T09:00:00Z')
    expect(queueStateLabel({ kind: 'paused', since: null }, iso)).toBe('paused')
  })
  it('explains a drain', () => {
    expect(queueStateLabel({ kind: 'draining' }, iso)).toBe('draining (server restarting)')
  })
})

describe('labels', () => {
  it('counts running against the limit and the waiting runs', () => {
    expect(capacityLabel(1, 4)).toBe('1 of 4 running')
    expect(queuedLabel(0)).toBe('nothing queued')
    expect(queuedLabel(3)).toBe('3 queued')
  })
})

describe('queueControl', () => {
  it('is hidden on a read-only server and before health is known', () => {
    expect(queueControl(view(), true)).toEqual({ kind: 'hidden' })
    expect(queueControl(view(), undefined)).toEqual({ kind: 'hidden' })
  })
  it('pauses an open queue and resumes a paused one', () => {
    expect(queueControl(view(), false)).toEqual({ kind: 'pause', disabled: false })
    expect(queueControl(view({ paused: true }), false)).toEqual({ kind: 'resume', disabled: false })
  })
  it('is disabled while the server drains', () => {
    expect(queueControl(view({ draining: true }), false)).toEqual({ kind: 'pause', disabled: true })
  })
})

describe('queueRows', () => {
  it('orders the waiting runs next first and links each to its run', () => {
    const rows = queueRows([
      entry('r2', { position: 2, target: 'a.py:x,a.py:y', command: 'run', triggered_by: 'schedule', schedule_id: 'a.py:x' }),
      entry('r1', { position: 1 }),
    ], iso)
    expect(rows.map((r) => r.runId)).toEqual(['r1', 'r2'])
    expect(rows[0]).toEqual({
      runId: 'r1',
      position: '#1',
      target: 'all assets',
      command: 'get',
      trigger: 'api',
      scheduleId: null,
      queuedAt: '2026-10-22T10:00:00Z',
      href: '/runs?run=r1',
    })
    expect(rows[1]).toMatchObject({ position: '#2', target: 'a.py:x,a.py:y', command: 'run', trigger: 'schedule', scheduleId: 'a.py:x' })
  })
  it('puts an entry without a position last', () => {
    expect(queueRows([entry('a', { position: null }), entry('b', { position: 1 })], iso).map((r) => r.position)).toEqual(['#1', '–'])
  })
})

describe('queuePollInterval', () => {
  it('polls faster while anything runs or waits', () => {
    expect(queuePollInterval(undefined)).toBe(QUEUE_POLL_IDLE_MS)
    expect(queuePollInterval(view())).toBe(QUEUE_POLL_IDLE_MS)
    expect(queuePollInterval(view({ queued: [entry('r1')] }))).toBe(QUEUE_POLL_BUSY_MS)
    expect(queuePollInterval(view({ running: [entry('r1', { status: 'running', position: null })] }))).toBe(QUEUE_POLL_BUSY_MS)
  })
})

describe('healthQueueLabel', () => {
  it('notes waiting runs and a pause; nothing for an empty open queue', () => {
    expect(healthQueueLabel(undefined)).toBeNull()
    expect(healthQueueLabel({ paused: false, queued: 0, running: 1, max_runs: 4 })).toBeNull()
    expect(healthQueueLabel({ paused: false, queued: 3, running: 4, max_runs: 4 })).toBe('3 queued')
    expect(healthQueueLabel({ paused: true, queued: 3, running: 0, max_runs: 4 })).toBe('3 queued · paused')
    expect(healthQueueLabel({ paused: true, queued: 0, running: 0, max_runs: 4 })).toBe('queue paused')
  })
})

describe('queueMatters', () => {
  const idle = { running: [], queued: [], paused: false, draining: false, capacity: null } as unknown as QueueView
  it('is nothing for an idle queue', () => expect(queueMatters(idle)).toBe(false))
  it('shows work, and a queue holding work back', () => {
    expect(queueMatters({ ...idle, running: [{}] } as unknown as QueueView)).toBe(true)
    expect(queueMatters({ ...idle, paused: true })).toBe(true)
    expect(queueMatters({ ...idle, draining: true })).toBe(true)
  })
})
