import { describe, it, expect } from 'vitest'
import { runFeedback } from './runFeedback'
import type { LogLine, StatusKind } from './types'

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
