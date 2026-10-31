import { describe, it, expect } from 'vitest'
import { runFeedback, runNotice, runOutcomeSummary } from './runFeedback'
import { runByStatus } from '@/kit/fixtures'
import type { LogLine, RunView, StatusKind } from './types'

const LOGS: LogLine[] = [{ nodeId: 'n', text: 'hello' }]

describe('runFeedback', () => {
  it('running → streaming with logs', () => {
    expect(runFeedback('running', LOGS, null)).toEqual({ kind: 'streaming', logs: LOGS })
  })

  it('success with logs → output', () => {
    expect(runFeedback('success', LOGS, null)).toEqual({ kind: 'output', logs: LOGS })
  })

  it('success with no logs → done (cache hit)', () => {
    expect(runFeedback('success', [], null)).toEqual({ kind: 'done' })
  })

  it('failed → failed, carrying the error message', () => {
    expect(runFeedback('failed', [], "No module named 'sklearn'")).toEqual({
      kind: 'failed',
      error: "No module named 'sklearn'",
      logs: [],
    })
  })

  it('failed with no error still surfaces a failed descriptor', () => {
    expect(runFeedback('failed', [], null)).toEqual({ kind: 'failed', error: null, logs: [] })
  })

  it.each<StatusKind>(['queued', 'pending', 'skipped', 'warning'])(
    'inert status %s → idle',
    (status) => {
      expect(runFeedback(status, [], null)).toEqual({ kind: 'idle' })
    },
  )

  it('every StatusKind yields a feedback (total over the union)', () => {
    const all: StatusKind[] = [
      'success',
      'running',
      'failed',
      'warning',
      'queued',
      'skipped',
      'pending',
    ]
    for (const s of all) {
      expect(() => runFeedback(s, [], null)).not.toThrow()
    }
  })
})

describe('runNotice', () => {
  const started = { runId: '127', target: 'p.py:demand_forecast' }
  const base = { target: 'p.py:demand_forecast', pending: false, error: null, started: null, run: null }

  it('shows nothing until a run is asked for', () => {
    expect(runNotice(base)).toBeNull()
  })

  it('follows the request: starting, then refused', () => {
    expect(runNotice({ ...base, pending: true })).toEqual({ kind: 'starting', target: 'p.py:demand_forecast' })
    expect(runNotice({ ...base, error: '503 draining' })).toEqual({
      kind: 'rejected', target: 'p.py:demand_forecast', error: '503 draining',
    })
  })

  it('an accepted run is queued until its record says it runs', () => {
    expect(runNotice({ ...base, started })).toEqual({ kind: 'queued', target: 'p.py:demand_forecast', runId: '127' })
    expect(runNotice({ ...base, started, run: runByStatus.pending })).toMatchObject({ kind: 'queued', runId: '127' })
    // Another run's record (a stale query) is not this run's.
    expect(runNotice({ ...base, started, run: { ...runByStatus.running, run_id: '9' } })).toMatchObject({ kind: 'queued' })
  })

  it('a live run reports its progress', () => {
    expect(runNotice({ ...base, started, run: runByStatus.running })).toEqual({
      kind: 'running', target: 'p.py:demand_forecast', runId: '127', done: 1, total: 4,
    })
  })

  it('a finished run reports its outcome and stays', () => {
    expect(runNotice({ ...base, started, run: runByStatus.success })).toEqual({
      kind: 'finished', target: 'p.py:demand_forecast', runId: '127', status: 'success', summary: 'succeeded in 42.3s',
    })
    expect(runNotice({ ...base, started, run: runByStatus.failed })).toMatchObject({
      kind: 'finished', status: 'failed', summary: "failed: orders_enriched: KeyError: 'currency'",
    })
    expect(runNotice({ ...base, started, run: runByStatus.cancelled })).toMatchObject({ status: 'skipped', summary: 'cancelled' })
  })

  it('the request comes before the record: a new request replaces a finished run', () => {
    expect(runNotice({ ...base, pending: true, started, run: runByStatus.success })).toEqual({
      kind: 'starting', target: 'p.py:demand_forecast',
    })
  })
})

describe('runOutcomeSummary', () => {
  it('counts failed keys from the per-node summary, however few steps are listed', () => {
    const run: RunView = {
      ...runByStatus.failed,
      steps_page: { total: 3077, truncated: true, nodes: [{ node_id: 'p.py:a', keys: 3077, failed: 7, elapsed_seconds: 2, calls: 5 }] },
    }
    expect(runOutcomeSummary(run)).toBe('7 keys failed')
  })

  it('counts failed keys from the listed steps otherwise, and names a single failure', () => {
    const key = (k: string) => ({ ...runByStatus.failed.steps[1]!, node_id: `p.py:a[k=${k}]`, partition_key: `k=${k}` })
    expect(runOutcomeSummary({ ...runByStatus.failed, steps: [key('1'), key('2')] })).toBe('2 keys failed')
    expect(runOutcomeSummary(runByStatus.error)).toBe('failed: could not start a worker: python3 not found')
    expect(runOutcomeSummary({ ...runByStatus.failed, error: null })).toBe('failed')
  })
})
