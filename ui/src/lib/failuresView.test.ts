import { describe, expect, it } from 'vitest'
import { INITIAL_FAILURES, failureTable, muteKey, parseMuted, updateFailures } from './failuresView'
import { replay } from './program'
import type { Failure } from './health'

const failure = (id: string, headline: string, impact: number | null, failingSince = 'then'): Failure => ({
  id, name: id, kind: 'asset', headline, error: headline, at: null, failingSince, failures: 1, impact, missingKeys: null,
})

describe('failures view', () => {
  const list = [failure('a', 'row count fell 64%', 2), failure('b', 'row count fell 12%', 5), failure('c', 'KeyError', 9)]

  it('heads each cause, biggest impact first, with a real message', () => {
    const { rows } = failureTable(list, INITIAL_FAILURES)
    expect(rows.map((r) => (r.kind === 'cause' ? r.group.cause : r.failure.id))).toEqual(['KeyError', 'c', 'row count fell #%', 'a', 'b'])
    expect(rows.flatMap((r) => (r.kind === 'cause' ? [r.group.headline] : []))).toEqual(['KeyError', 'row count fell 64%'])
  })

  it('hides a muted failure until asked, and counts it', () => {
    const key = muteKey(list[2]!)
    const { model, effects } = replay(updateFailures, INITIAL_FAILURES, [{ type: 'toggle_mute', key }])
    expect(effects).toEqual([{ type: 'save_muted', muted: [key] }])
    const hidden = failureTable(list, model)
    expect(hidden.mutedCount).toBe(1)
    expect(hidden.rows.some((r) => r.kind === 'failure' && r.failure.id === 'c')).toBe(false)
    const shown = failureTable(list, updateFailures(model, { type: 'toggle_show_muted' })[0])
    expect(shown.rows.some((r) => r.kind === 'failure' && r.muted)).toBe(true)
    expect(updateFailures(model, { type: 'toggle_mute', key })[0].muted).toEqual([])
  })

  it('a new spell of failing is not muted', () => {
    expect(muteKey(failure('a', 'x', 0, 'later'))).not.toBe(muteKey(failure('a', 'x', 0, 'then')))
  })

  it('asks for each action and reports it', () => {
    expect(updateFailures(INITIAL_FAILURES, { type: 'retry', id: 'a', kind: 'task', name: 'a' })[1]).toEqual([{ type: 'retry', id: 'a', kind: 'task', name: 'a' }])
    expect(updateFailures(INITIAL_FAILURES, { type: 'action_failed', text: 'no' })[0].notice).toEqual({ text: 'no', failed: true })
    // A failed key names the run that recorded it; a node's run is looked up.
    expect(updateFailures(INITIAL_FAILURES, { type: 'open_run', id: 'a', name: 'a', runId: 'd9becbdf4a92' })[1]).toEqual([{ type: 'open_run', id: 'a', name: 'a', runId: 'd9becbdf4a92' }])
  })

  it('reads storage defensively', () => {
    expect(parseMuted('["a",1]')).toEqual(['a'])
    expect(parseMuted('{oops')).toEqual([])
    expect(parseMuted(null)).toEqual([])
  })
})
