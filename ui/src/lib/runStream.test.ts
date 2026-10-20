import { describe, it, expect } from 'vitest'
import { reduceRunEvent, endRunStream, isNewFrame, EMPTY_RUN_STATE, type RunStreamState } from './runStream'
import type { RunEvent } from './types'

/** Fold a sequence of events from the empty state. */
function play(events: RunEvent[]): RunStreamState {
  return events.reduce(reduceRunEvent, EMPTY_RUN_STATE)
}

const NODE = 'iris_project/assets.py:raw_data'

describe('reduceRunEvent', () => {
  it('streams logs and settles a node to success (happy path)', () => {
    const state = play([
      { type: 'run_started', run_id: 'r1' },
      { type: 'log', node_id: NODE, line: 'loading…' },
      { type: 'log', node_id: NODE, line: 'done' },
      { type: 'step_finished', node_id: NODE, ok: true, elapsed_seconds: 1.2 },
      { type: 'run_finished', run_id: 'r1', ok: true },
    ])

    expect(state.logs.map((l) => l.text)).toEqual(['loading…', 'done'])
    expect(state.statuses[NODE]).toBe('success')
    expect(state.errors[NODE]).toBeUndefined()
    expect(state.running).toBe(false)
    expect(state.ok).toBe(true)
  })

  it('captures the error and marks the node failed (failure path)', () => {
    // This is the exact shape barca emits when a worker import fails, e.g.
    // "No module named 'sklearn'" — no logs, then a failed step.
    const state = play([
      { type: 'run_started', run_id: 'r2' },
      { type: 'step_finished', node_id: NODE, ok: false, error: "No module named 'sklearn'" },
      { type: 'run_finished', run_id: 'r2', ok: false },
    ])

    expect(state.logs).toEqual([])
    expect(state.statuses[NODE]).toBe('failed')
    expect(state.errors[NODE]).toBe("No module named 'sklearn'")
    expect(state.running).toBe(false)
    expect(state.ok).toBe(false)
  })

  it('marks a node running on its first log line', () => {
    const state = play([
      { type: 'run_started', run_id: 'r3' },
      { type: 'log', node_id: NODE, line: 'working' },
    ])
    expect(state.statuses[NODE]).toBe('running')
    expect(state.running).toBe(true)
  })

  it('cache-hit run finishes ok with no step events and no logs', () => {
    const state = play([
      { type: 'run_started', run_id: 'r4' },
      { type: 'run_finished', run_id: 'r4', ok: true },
    ])
    expect(state.logs).toEqual([])
    expect(state.statuses).toEqual({})
    expect(state.ok).toBe(true)
    expect(state.running).toBe(false)
  })

  it('is pure — does not mutate the input state', () => {
    const before = EMPTY_RUN_STATE
    const after = reduceRunEvent(before, { type: 'log', node_id: NODE, line: 'x' })
    expect(before.logs).toEqual([])
    expect(after).not.toBe(before)
  })

  it('keeps progress lines apart from step logs', () => {
    const state = play([
      { type: 'run_started', run_id: 'r5' },
      { type: 'line', line: 'barca get: 3 steps', when: 'always' },
      { type: 'line', line: 'step raw_data started', when: 'agent' },
      { type: 'log', node_id: NODE, line: 'hello' },
    ])
    expect(state.lines).toEqual([
      { line: 'barca get: 3 steps', when: 'always' },
      { line: 'step raw_data started', when: 'agent' },
    ])
    expect(state.logs.map((l) => l.text)).toEqual(['hello'])
    expect(state.statuses[NODE]).toBe('running')
  })

  it('marks a step served from cache as a cached success', () => {
    const state = play([
      { type: 'run_started', run_id: 'r6' },
      { type: 'step_finished', node_id: NODE, ok: true, cached: true },
      { type: 'step_finished', node_id: 'p.py:other', ok: true, elapsed_seconds: 0.4 },
    ])
    expect(state.statuses[NODE]).toBe('success')
    expect(state.cached).toEqual({ [NODE]: true })
    expect(state.errors).toEqual({})
  })

  it('keeps a successful outcome from run_finished', () => {
    const state = play([
      { type: 'run_started', run_id: 'r7' },
      {
        type: 'run_finished',
        run_id: 'r7',
        ok: true,
        outcome: { status: 'success', run_id: 'r7', elapsed_seconds: 0.1, steps_executed: 1, phases: 1, steps: [], warnings: [], final_output: 42 },
      },
    ])
    expect(state.running).toBe(false)
    expect(state.outcome).toEqual({ status: 'success', run_id: 'r7', final_output: 42 })
  })

  it('keeps a failed outcome and its error envelope', () => {
    const envelope = {
      error: "step raw_data failed: No module named 'sklearn'",
      code: 1,
      kind: 'step_failed',
      remediation: 'install the missing module',
      node: NODE,
    }
    const state = play([
      { type: 'run_started', run_id: 'r8' },
      { type: 'step_finished', node_id: NODE, ok: false, error: "No module named 'sklearn'" },
      { type: 'run_finished', run_id: 'r8', ok: false, outcome: { status: 'failed', run_id: 'r8', envelope } },
    ])
    expect(state.ok).toBe(false)
    expect(state.outcome).toMatchObject({ status: 'failed', envelope: { kind: 'step_failed', remediation: 'install the missing module', node: NODE } })
  })

  it('a cancelled run ends not running, with its envelope', () => {
    const state = play([
      { type: 'run_started', run_id: 'r9' },
      {
        type: 'run_finished', run_id: 'r9', ok: false,
        outcome: { status: 'cancelled', envelope: { error: 'cancelled', code: 130, kind: 'cancelled', remediation: null } },
      },
    ])
    expect(state.running).toBe(false)
    expect(state.outcome?.status).toBe('cancelled')
  })

  it('run_finished without an outcome leaves it null', () => {
    const state = play([{ type: 'run_finished', run_id: 'r10', ok: true }])
    expect(state.outcome).toBeNull()
  })
})

describe('endRunStream', () => {
  it('stops a running stream and keeps what it saw', () => {
    const running = play([{ type: 'run_started', run_id: 'r' }, { type: 'log', node_id: NODE, line: 'x' }])
    const ended = endRunStream(running)
    expect(ended.running).toBe(false)
    expect(ended.ok).toBeNull()
    expect(ended.logs).toEqual(running.logs)
  })
  it('leaves a finished stream as it is', () => {
    const done = play([{ type: 'run_finished', run_id: 'r', ok: true }])
    expect(endRunStream(done)).toBe(done)
  })
})

describe('isNewFrame', () => {
  it('applies the first frame and every later id', () => {
    expect(isNewFrame(null, '0')).toBe(true)
    expect(isNewFrame(3, '4')).toBe(true)
  })
  it('skips frames a reconnect replays', () => {
    expect(isNewFrame(3, '3')).toBe(false)
    expect(isNewFrame(3, '0')).toBe(false)
  })
  it('applies frames without a numeric id', () => {
    expect(isNewFrame(3, '')).toBe(true)
    expect(isNewFrame(3, 'x')).toBe(true)
  })
})
