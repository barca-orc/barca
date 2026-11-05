import { describe, expect, it } from 'vitest'
import { INITIAL_FAILURES, failureTable, updateFailures } from './failuresView'
import type { Failure } from './health'

const failure = (id: string, headline: string, impact: number | null, failingSince = 'then'): Failure => ({
  id, name: id, kind: 'asset', headline, error: headline, at: null, failingSince, failures: 1, impact, missingKeys: null,
})

describe('failures view', () => {
  const list = [failure('a', 'row count fell 64%', 2), failure('b', 'row count fell 12%', 5), failure('c', 'KeyError', 9)]

  it('heads each cause, biggest impact first, with a real message, and hides nothing', () => {
    const { rows } = failureTable(list)
    expect(rows.map((r) => (r.kind === 'cause' ? r.group.cause : r.failure.id))).toEqual(['KeyError', 'c', 'row count fell #%', 'a', 'b'])
    expect(rows.flatMap((r) => (r.kind === 'cause' ? [r.group.headline] : []))).toEqual(['KeyError', 'row count fell 64%'])
    expect(rows.filter((r) => r.kind === 'failure')).toHaveLength(list.length)
  })

  it('asks for each action and reports it', () => {
    expect(updateFailures(INITIAL_FAILURES, { type: 'retry', id: 'a', kind: 'task', name: 'a' })[1]).toEqual([{ type: 'retry', id: 'a', kind: 'task', name: 'a' }])
    expect(updateFailures(INITIAL_FAILURES, { type: 'action_failed', text: 'no' })[0].notice).toEqual({ text: 'no', failed: true })
    expect(updateFailures(INITIAL_FAILURES, { type: 'done', text: 'Error copied' })[0].notice).toEqual({ text: 'Error copied', failed: false })
    // A failed key names the run that recorded it; a node's run is looked up.
    expect(updateFailures(INITIAL_FAILURES, { type: 'open_run', id: 'a', name: 'a', runId: 'd9becbdf4a92' })[1]).toEqual([{ type: 'open_run', id: 'a', name: 'a', runId: 'd9becbdf4a92' }])
  })
})
