import { describe, it, expect } from 'vitest'
import { severityOf } from '@/lib/assetTable'
import { runFeedback } from '@/lib/runFeedback'
import { parseOutcome, parseRunStatus } from '@/lib/runs'
import { runningStatus } from '@/lib/running'
import { feedbackInputs, nodeBySeverity, runByStatus, runningByStep, variants } from './fixtures'

// The tables' keys are checked by the compiler; these check each value is the state its key
// names, so the kit never shows one state under another's label.
describe('kit fixtures', () => {
  it('has a node in each severity', () => {
    for (const severity of variants(nodeBySeverity)) expect(severityOf(nodeBySeverity[severity])).toBe(severity)
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
