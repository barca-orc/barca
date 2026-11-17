import { describe, expect, it } from 'vitest'
import {
  attemptsLabel, cancellationNotice, groupOfNodes, inFlightSummary, planExplanation, reasonLabel,
  reasonSummary, runOverview, slowestNodes, stepsPageNote, triggerDetail,
} from './runExplain'
import type { ActiveStep, PlanReasons, ReasonCategory, RunView } from './types'

const NOW = Date.parse('2026-11-16T12:00:00Z')
const ago = (s: number) => new Date(NOW - s * 1000).toISOString()

const plan: PlanReasons = {
  nodes: 469,
  steps: 5_492,
  omitted: 2,
  reasons: [
    { category: 'new_keys', subject: null, nodes: 1, steps: 5_000, example: 'billing.py:invoices_by_account' },
    { category: 'code_changed', subject: 'helper billing.py:_n', nodes: 29, steps: 29, example: 'billing.py:dunning_05' },
    { category: 'inputs_changed', subject: 'input billing.py:dunning_05', nodes: 3, steps: 3, example: 'billing.py:marts_01' },
    { category: 'code_changed', subject: 'code', nodes: 2, steps: 2, example: 'crm.py:owners_00' },
  ],
}

describe('planExplanation', () => {
  it('groups the reasons by category, most steps first, and keeps each subject', () => {
    const e = planExplanation(plan, false)
    if (e.kind !== 'reasons') throw new Error('expected reasons')
    expect(e.headline).toBe('5,492 steps of 469 nodes were not cache hits')
    expect(e.groups.map((g) => g.category)).toEqual(['new_keys', 'code_changed', 'inputs_changed'])
    expect(e.groups[1]!.rows.map((r) => r.subject)).toEqual(['helper billing.py:_n', 'code'])
    // The input's word is the category's; the subject names the node.
    expect(e.groups[2]!.rows[0]!.subject).toBe('billing.py:dunning_05')
    expect(e.groups[0]!.rows[0]!.counts).toBe('5,000 steps · 1 node')
    expect(e.groups[0]!.rows[0]!.share).toBeCloseTo(5_000 / 5_492)
    expect(e.omitted).toBe(2)
    expect(e.partial).toBe(false)
  })

  it('says a live run has decided only some phases so far', () => {
    const e = planExplanation(plan, true)
    expect(e.kind === 'reasons' && e.headline).toBe('5,492 steps of 469 nodes decided to run so far')
  })

  it('has a note, not an empty list, when nothing was decided to run', () => {
    expect(planExplanation(null, true)).toEqual({ kind: 'none', note: 'No step decided to run yet.' })
    expect(planExplanation({ ...plan, steps: 0, reasons: [] }, false).kind).toBe('none')
  })

  it('words every category', () => {
    const all: ReasonCategory[] = ['never_run', 'new_keys', 'failed', 'code_changed', 'inputs_changed', 'env_changed', 'changed',
      'refresh', 'refresh_cascade', 'refresh_all', 'artifact_missing', 'evicted', 'task', 'sensor', 'not_examined']
    const words = all.map(reasonLabel)
    expect(new Set(words).size).toBe(all.length)
  })

  it('summarizes the first reasons in one line', () => {
    expect(reasonSummary(plan)).toBe('5,000 new partition keys · 29 code changed (helper billing.py:_n)')
    expect(reasonSummary(null)).toBeNull()
  })
})

describe('who and how', () => {
  it('names the token that asked for a run', () => {
    expect(triggerDetail('api', 'token:ci-bot')).toBe('api · token:ci-bot')
    expect(triggerDetail('schedule', null)).toBe('schedule')
    expect(triggerDetail(null, null)).toBe('unknown')
  })

  it('labels attempts against steps: the retries are the difference', () => {
    expect(attemptsLabel(19_461, 62)).toBe('19,461 attempts · 62 retries')
    expect(attemptsLabel(1, 0)).toBe('1 attempt')
    expect(attemptsLabel(3, 1)).toBe('3 attempts · 1 retry')
    expect(attemptsLabel(null, null)).toBeNull()
    expect(attemptsLabel(0, 0)).toBeNull()
  })

  it('says a run cancelled in the queue never started, and what it waited for', () => {
    const same = cancellationNotice({ by: 'request', principal: 'token:ops', started: false, waiting_behind: ['a', 'b'], same_as: 'b' })
    expect(same).toEqual({
      headline: 'Not started: the same target was already running as run b.',
      detail: 'Cancelled by token:ops while it waited in the queue; it executed nothing.',
      run: 'b',
    })
    const behind = cancellationNotice({ by: 'request', principal: null, started: false, waiting_behind: ['a'], same_as: null })
    expect(behind?.headline).toBe('Not started: cancelled while it waited in the queue.')
    expect(behind?.detail).toBe('It waited behind run a. Cancelled.')
    expect(cancellationNotice({ by: 'shutdown', principal: null, started: true, waiting_behind: [], same_as: null })?.headline)
      .toBe('Cancelled by the server stopping while it ran.')
    expect(cancellationNotice(null)).toBeNull()
  })
})

const steps = (n: number) => Array.from({ length: n }, (_, i) => ({
  node_id: `p.py:n${i}`, status: 'success', elapsed_seconds: i + 1, error: null, partition_key: null, attempts: 1,
  run_hash: null, artifact_path: null, artifact_format: null, artifact_size_bytes: null, rows: null, error_type: null,
  error_message: null, error_traceback: null, error_phase: null, worker_exit: null, cpu_seconds: null,
  max_rss_bytes: null, output_tail: null, batch: null,
})) satisfies RunView['steps']

describe('runOverview', () => {
  const active: ActiveStep[] = [
    { node_id: 'b.py:keys', step: 'running', partitions: { running: 1880, done: 3120, total: 5000 } },
    { node_id: 'b.py:one', step: 'running', partitions: null },
    { node_id: 'c.py:later', step: 'queued', partitions: null },
  ]
  const groups = [
    { id: 'g:billing', name: 'billing', description: '', members: ['b.py:keys', 'b.py:one', 'c.py:later'], output: '' },
    { id: 'g:billing.raw', name: 'billing.raw', description: '', members: ['b.py:keys'], output: '' },
  ]

  it('says how far, what runs now, by group, and an ETA from the rate so far', () => {
    const o = runOverview({
      started_at: ago(600), steps_total: 23_278, steps_executed: 9_000, steps_cached: 0,
      steps: steps(3), steps_page: { total: 9_000, truncated: true },
    }, active, groups, NOW)
    expect(o.progress).toEqual({ done: 9_000, total: 23_278, percent: 38, executed: 9_000, cached: 0, keys: { done: 3120, total: 5000 } })
    expect(o.elapsed).toBe('10m 0s')
    // 14,278 steps left at 15 a second.
    expect(o.eta).toBe('about 16m left')
    expect(o.running.map((r) => r.nodeId)).toEqual(['b.py:keys', 'b.py:one'])
    expect(o.running[0]!.keys).toBe('3,120 of 5,000 keys')
    expect(o.queued).toBe(1)
    expect(o.groups).toEqual([{ group: 'billing', running: 1, queued: 1 }, { group: 'billing.raw', running: 1, queued: 0 }])
    expect(o.slowest.map((s) => s.nodeId)).toEqual(['p.py:n2', 'p.py:n1', 'p.py:n0'])
  })

  it('has no ETA before a step executed, and cached steps are done', () => {
    const o = runOverview({
      started_at: ago(5), steps_total: 10, steps_executed: 0, steps_cached: 4, steps: [], steps_page: { total: 0, truncated: false },
    }, [], [], NOW)
    expect(o.eta).toBeNull()
    expect(o.progress.percent).toBe(40)
    expect(o.progress.keys).toBeNull()
  })
})

describe('helpers', () => {
  it('takes the innermost group of a node', () => {
    const g = groupOfNodes([
      { id: 'a', name: 'all', description: '', members: ['x', 'y'], output: '' },
      { id: 'b', name: 'inner', description: '', members: ['x'], output: '' },
    ])
    expect(g.get('x')).toBe('inner')
    expect(g.get('y')).toBe('all')
  })

  it('ranks the slowest nodes by their summed time, the server summary first', () => {
    const nodes = [
      { node_id: 'a', keys: 10, failed: 0, elapsed_seconds: 3, calls: 0 },
      { node_id: 'b', keys: 1, failed: 0, elapsed_seconds: 9, calls: 0 },
    ]
    expect(slowestNodes(nodes, steps(5)).map((s) => s.nodeId)).toEqual(['b', 'a'])
    expect(slowestNodes([], steps(7), 2).map((s) => s.nodeId)).toEqual(['p.py:n6', 'p.py:n5'])
  })

  it('says which steps a page lists, never a silent cap', () => {
    expect(stepsPageNote({ total: 19_399, truncated: true, offset: 1_000, next_offset: 2_000 }, 1_000))
      .toBe('Showing steps 1,001–2,000 of 19,399 (failed steps first).')
    expect(stepsPageNote({ total: 5, truncated: false }, 5)).toBeNull()
  })

  it('sums up a large run in flight in one line', () => {
    const s = inFlightSummary({
      steps_total: 23_278, steps_executed: 19_399, steps_cached: 0, started_at: ago(360), triggered_by: 'api', principal: 'token:ci',
    }, NOW)
    expect(s).toEqual({ label: 'running 23,278 steps · 83% · started 6m 0s ago by token:ci', percent: 83, large: true })
    const small = inFlightSummary({ steps_total: null, steps_executed: 0, steps_cached: 0, started_at: ago(0), triggered_by: null, principal: null }, NOW)
    expect(small).toEqual({ label: 'running · started 0s ago', percent: null, large: false })
  })
})

describe('a run from an older server', () => {
  it('reads the fields it does not send as absent', () => {
    expect(cancellationNotice(undefined)).toBeNull()
    expect(planExplanation(undefined, false).kind).toBe('none')
    expect(planExplanation(undefined, false, 4976)).toEqual({ kind: 'none', note: '4,976 steps ran, but this run recorded no reasons (barca records them from 0.43).' })
    expect(planExplanation(undefined, false, 0)).toEqual({ kind: 'none', note: 'Every step was served from cache.' })
    expect(triggerDetail('api', undefined)).toBe('api')
    expect(attemptsLabel(3, undefined)).toBe('3 attempts')
  })
})
