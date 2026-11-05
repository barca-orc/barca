import { describe, it, expect } from 'vitest'
import { lastAttemptFailed } from '@/lib/cacheStateDisplay'
import { runFeedback } from '@/lib/runFeedback'
import { parseOutcome, parseRunStatus } from '@/lib/runs'
import { runningStatus } from '@/lib/running'
import { failedNode, failedTask, feedbackInputs, nodeByState, runByStatus, runningByStep, variants } from './fixtures'

// The tables' keys are checked by the compiler; these check each value is the state its key
// names, so the kit never shows one state under another's label.
describe('kit fixtures', () => {
  it('has a node in each cache state, none of which failed last', () => {
    for (const state of variants(nodeByState)) {
      expect(nodeByState[state].cache.state).toBe(state)
      expect(lastAttemptFailed(nodeByState[state])).toBe(false)
    }
    expect(lastAttemptFailed(failedNode)).toBe(true)
    expect(lastAttemptFailed(failedTask)).toBe(true)
  })

  it('has a running node at each step', () => {
    for (const step of variants(runningByStep)) expect(runningStatus(runningByStep[step])).toBe(step)
  })

  it('has inspector inputs for each feedback', () => {
    for (const kind of variants(feedbackInputs)) {
      const { status, logs, error } = feedbackInputs[kind]
      expect(runFeedback(status, logs, error).kind).toBe(kind)
    }
  })

  it('has a run in each status, with an outcome this UI reads', () => {
    for (const status of variants(runByStatus)) {
      const run = runByStatus[status]
      expect(parseRunStatus(run.status)).toBe(status)
      if (run.outcome) expect(parseOutcome(run.outcome)).not.toBeNull()
    }
  })
})
