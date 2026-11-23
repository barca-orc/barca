import { describe, it, expect } from 'vitest'
import { runFeedback, runNotice, runOutcomeSummary } from './runFeedback'
import { runInfrastructureFailure, runByStatus } from '@/kit/fixtures'
import { RUN_IDS, runDetail } from './testdata/project'
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

  it.each<StatusKind>(['queued', 'skipped', 'cancelled'])(
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
      'queued',
      'skipped',
      'cancelled',
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
    expect(runNotice({ ...base, started })).toEqual({ kind: 'queued', target: 'p.py:demand_forecast', runId: '127', restart: false })
    expect(runNotice({ ...base, started, run: runByStatus.pending })).toMatchObject({ kind: 'queued', runId: '127', restart: false })
    // Accepted by a draining server: it starts on the next one (#1011).
    expect(runNotice({ ...base, started, run: { ...runByStatus.pending, run: { ...runByStatus.pending.run, waiting_for: 'restart' } } })).toMatchObject({ kind: 'queued', restart: true })
    // Another run's record (a stale query) is not this run's.
    expect(runNotice({ ...base, started, run: { ...runByStatus.running, run: { ...runByStatus.running.run, run_id: '9' } } })).toMatchObject({ kind: 'queued' })
  })

  it('a live run reports its progress', () => {
    expect(runNotice({ ...base, started, run: runByStatus.running })).toEqual({
      kind: 'running', target: 'p.py:demand_forecast', runId: '127', done: 1, total: 4, control: runByStatus.running.run.control,
    })
  })

  it('a finished run reports its outcome and stays', () => {
    // Two runs of the generated project, as `GET /runs/{id}` serves them.
    const of = (id: string) => ({ ...base, started: { ...started, runId: id }, run: runDetail(id) })
    expect(runNotice(of(RUN_IDS.first))).toEqual({
      kind: 'finished', target: 'p.py:demand_forecast', runId: RUN_IDS.first, status: 'success', summary: 'succeeded in 42.3s',
    })
    expect(runNotice(of(RUN_IDS.failed))).toMatchObject({
      kind: 'finished', status: 'failed', summary: 'failed: ValueError: 3 rows with a negative total',
    })
    // One key failed: the run's error names it; the count is said from two up.
    expect(runNotice(of(RUN_IDS.failedKey))).toMatchObject({ status: 'failed', summary: "failed: KeyError: 'units'" })
    expect(runNotice({ ...base, started, run: runByStatus.cancelled })).toMatchObject({ status: 'cancelled', summary: 'cancelled' })
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
    // A run whose answer carries no per-node summary: the failed keys among the steps listed.
    const listed = { ...runByStatus.failed, steps_page: { total: 2, truncated: false } }
    const key = (k: string) => ({ ...runByStatus.failed.steps[0]!, node_id: `p.py:a[k=${k}]`, partition_key: `k=${k}` })
    expect(runOutcomeSummary({ ...listed, steps: [key('1'), key('2')] })).toBe('2 keys failed')
    expect(runOutcomeSummary(runInfrastructureFailure)).toBe('failed: could not start a worker: python3 not found')
    expect(runOutcomeSummary({ ...listed, run: { ...listed.run, error: null, failure: null } })).toBe('failed')
  })
})
