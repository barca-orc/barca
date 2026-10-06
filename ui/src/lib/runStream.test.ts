import { describe, it, expect } from 'vitest'
import { reduceRunEvent, EMPTY_RUN_STATE, type RunStreamState } from './runStream'
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
})
