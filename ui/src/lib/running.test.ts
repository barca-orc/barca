import { describe, expect, it } from 'vitest'
import { counts } from './testdata/rollup'
import { graphState, memberHealth } from './graphState'
import { groupHealth } from './groups'
import { pipelineSummaries } from './pipeline'
import { buildRows } from './assetTable'
import {
  POLL_IDLE_MS,
  POLL_RUNNING_MS,
  POLL_RUNNING_LARGE_MS,
  LARGE_SCOPE,
  elapsedLabel,
  runningBadge,
  runningCount,
  runsInProgress,
  runsInProgressLabel,
  statePollInterval,
} from './running'
import { statusMeta } from './status'
import type { NodeRunning, NodeState } from './types'
import { served } from './testdata/served'

const STARTED = '2026-10-21T10:00:00.000Z'
const NOW = Date.parse(STARTED) + 75_000

function run(overrides: Partial<NodeRunning> = {}): NodeRunning {
  return {
    run_id: 'r1',
    triggered_by: 'schedule',
    started_at: STARTED,
    step: 'running',
    partitions: null,
    ...overrides,
  }
}

function node(id: string, running: NodeRunning | null = null, failed = false): NodeState {
  return served({
    id,
    name: id.split(':').pop()!,
    kind: 'asset',
    inputs: [],
    partitioned: running?.partitions != null,
    cache: { state: 'cached', reason: 'materialized', detail: 'cached' },
    last_materialization: failed
      ? {
          status: 'failed',
          created_at: '2026-10-06 10:00:00',
          elapsed_seconds: null,
          run_hash: null,
          artifact: null,
          format: null,
          size_bytes: null,
          rows: null,
          error: 'boom',
        }
      : null,
    shape: null,
    env: [],
    durations: null,
    next_run: null, prev_run: null, late_since: null, signals: [], last_success_at: null, failing_since: null, consecutive_failures: 0,
    running,
  })
}

describe('running state from /state', () => {
  it('pulses a node a run is working on, whoever started it, over its resting state', () => {
    for (const triggered_by of ['schedule', 'api', 'manual', null] as const) {
      const state = graphState(node('p.py:a', run({ triggered_by }), true))
      expect(state).toMatchObject({ kind: 'run', status: 'running', tone: 'running', label: 'running' })
      expect(statusMeta('running').live).toBe(true)
    }
    expect(graphState(node('p.py:a', run())).hint).toBe('Run r1, started by the scheduler')
    expect(graphState(node('p.py:a', run({ triggered_by: 'manual' }))).hint).toContain('the CLI')
    // A step waiting for its upstream is queued: shown, but still.
    const queued = graphState(node('p.py:a', run({ step: 'queued' })))
    expect(queued).toMatchObject({ kind: 'run', status: 'queued', tone: 'queued', label: 'queued' })
    expect(statusMeta('queued').live).toBe(false)
    // No run: the resting state, in its own word.
    expect(graphState(node('p.py:a'))).toMatchObject({ kind: 'cache', state: 'cached', label: 'fresh', tone: 'success' })
  })

  it('shows a partitioned node’s keys done', () => {
    const partitioned = run({ partitions: { running: 6, done: 4, total: 10 } })
    expect(graphState(node('p.py:k', partitioned)).label).toBe('running · 4/10 keys')
    expect(memberHealth(node('p.py:k', partitioned)).label).toBe('running · 4/10 keys')
  })

  it('gives the assets list a Running badge with the elapsed time and the run link', () => {
    expect(runningBadge(run(), NOW)).toEqual({
      tone: 'running',
      glyph: 'running',
      label: 'running · 1m 15s',
      hint: 'Run r1, started by the scheduler',
      href: '/runs?run=r1',
    })
    const keyed = runningBadge(run({ run_id: 'a b', partitions: { running: 1, done: 2, total: 3 } }), NOW)
    expect(keyed.label).toBe('running · 2/3 keys · 1m 15s')
    expect(keyed.href).toBe('/runs?run=a%20b')
  })

  it('formats elapsed time', () => {
    expect(elapsedLabel(STARTED, Date.parse(STARTED) + 9_000)).toBe('9s')
    expect(elapsedLabel(STARTED, Date.parse(STARTED) + 3_725_000)).toBe('1h 2m')
    // A clock a little behind the server's never shows a negative time.
    expect(elapsedLabel(STARTED, Date.parse(STARTED) - 5_000)).toBe('0s')
    expect(elapsedLabel('not a time', NOW)).toBe('')
  })

  it('counts runs in progress for the header chip', () => {
    const nodes = [
      node('p.py:a', run({ run_id: 'r1' })),
      node('p.py:b', run({ run_id: 'r1' })),
      node('q.py:c', run({ run_id: 'r2', triggered_by: 'manual' })),
      node('q.py:d'),
    ]
    expect(runsInProgress(nodes)).toEqual(['r1', 'r2'])
    expect(runsInProgressLabel(2)).toBe('2 runs in progress')
    expect(runsInProgressLabel(1)).toBe('1 run in progress')
    expect(runsInProgress([node('p.py:a')])).toEqual([])
  })

  it('counts running members per pipeline and per group, and the group pulses', () => {
    const nodes = [
      node('p.py:a', run()),
      node('p.py:b', null, true),
      node('q.py:c'),
    ]
    const summaries = pipelineSummaries(buildRows(nodes, 0), nodes, 'asset')
    expect(summaries.map((p) => [p.file, p.running])).toEqual([
      ['p.py', 1],
      ['q.py', 0],
    ])
    const byId = new Map(nodes.map((n) => [n.id, n]))
    expect(runningCount(['p.py:a', 'p.py:b', 'q.py:c'], byId)).toBe(1)
    // A failure inside a running group leads: running is shown beside it, never instead (#925).
    const group = groupHealth({ health: counts({ failing: 1, ok: 1 }), nodes: 2, running: 1 })
    expect(group).toMatchObject({ kind: 'group', tone: 'failed', label: '1 failing' })
    expect(group.kind === 'group' && group.parts?.map((p) => p.text)).toEqual(['1 failing', '1 running', '1 ok'])
    // A group with run work and nothing failed pulses as a run.
    expect(groupHealth({ health: counts({ ok: 1 }), nodes: 1, running: 1 })).toMatchObject({ kind: 'run', tone: 'running', label: '1 running' })
    // Once the run ends, the failed last attempt is the one fact left.
    expect(groupHealth({ health: counts({ failing: 1, ok: 1 }), nodes: 2, running: 0 })).toMatchObject({ tone: 'failed', label: '1 failing' })
  })

  it('polls /state every 2s while anything runs, every 10s otherwise', () => {
    expect(POLL_RUNNING_MS).toBe(2_000)
    expect(POLL_IDLE_MS).toBe(10_000)
    expect(statePollInterval(undefined)).toBe(POLL_IDLE_MS)
    expect(statePollInterval([node('p.py:a')])).toBe(POLL_IDLE_MS)
    expect(statePollInterval([node('p.py:a'), node('p.py:b', run())])).toBe(POLL_RUNNING_MS)
  })

  it('polls a big scope less often while it runs', () => {
    const many = Array.from({ length: LARGE_SCOPE }, (_, i) => node(`p.py:n${i}`))
    expect(statePollInterval(many)).toBe(POLL_IDLE_MS)
    expect(statePollInterval([...many.slice(1), node('p.py:busy', run())])).toBe(POLL_RUNNING_LARGE_MS)
  })
})
