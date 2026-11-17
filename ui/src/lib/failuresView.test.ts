import { describe, expect, it } from 'vitest'
import { INITIAL_FAILURES, failureTable, updateFailures } from './failuresView'
import type { Failure } from './health'

const failure = (id: string, headline: string, held: number, failingSince = 'then'): Failure => ({
  id, name: id, kind: 'asset', headline, error: headline, group: headline.replace(/\d+/g, '#'), at: null, failingSince, failures: 1, held, heldAssets: held, missingKeys: null,
})

describe('failures view', () => {
  // As the rollup serves its roots: the failure holding the most nodes first.
  const list = [failure('c', 'KeyError', 9), failure('b', 'row count fell 12%', 5), failure('a', 'row count fell 64%', 2), { ...failure('d', '3 of 8 keys failed', 1), group: null }]

  it('heads each error group where its first failure stands, in served order, and hides nothing', () => {
    const { rows } = failureTable(list)
    expect(rows.map((r) => (r.kind === 'cause' ? r.key : r.failure.id))).toEqual(['KeyError', 'c', 'row count fell #%', 'b', 'a', 'node:d', 'd'])
    expect(rows.flatMap((r) => (r.kind === 'cause' ? [[r.headline, r.failed]] : []))).toEqual([['KeyError', 1], ['row count fell 12%', 2], ['3 of 8 keys failed', 1]])
    expect(rows.filter((r) => r.kind === 'failure')).toHaveLength(list.length)
  })

  it('says an error once: a row under a heading with the same words does not repeat it', () => {
    const { rows } = failureTable(list)
    const same = Object.fromEntries(rows.flatMap((r) => (r.kind === 'failure' ? [[r.failure.id, r.sameError]] : [])))
    // `b` heads its group; `a` differs in its numbers, so its own words are shown.
    expect(same).toEqual({ c: true, b: true, a: false, d: true })
  })

  it('asks for each action and reports it', () => {
    expect(updateFailures(INITIAL_FAILURES, { type: 'retry', id: 'a', kind: 'task', name: 'a' })[1]).toEqual([{ type: 'retry', id: 'a', kind: 'task', name: 'a' }])
    expect(updateFailures(INITIAL_FAILURES, { type: 'action_failed', text: 'no' })[0].notice).toEqual({ text: 'no', failed: true })
    expect(updateFailures(INITIAL_FAILURES, { type: 'done', text: 'Error copied' })[0].notice).toEqual({ text: 'Error copied', failed: false })
    // A failed key names the run that recorded it; a node's run is looked up.
    expect(updateFailures(INITIAL_FAILURES, { type: 'open_run', id: 'a', name: 'a', runId: 'd9becbdf4a92' })[1]).toEqual([{ type: 'open_run', id: 'a', name: 'a', runId: 'd9becbdf4a92' }])
  })
})
