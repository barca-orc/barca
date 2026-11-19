import { describe, it, expect } from 'vitest'
import { graphState } from './graphState'
import { CACHE_STATES } from './cacheStateDisplay'
import { mapPartLabel, mapPartMeaning, mapPartOf } from './causeDisplay'
import type { NodeState } from './types'
import { blockedBy, served } from './testdata/served'
const state = (cache: NodeState['cache']['state'], last: 'failed' | 'success' | null = 'success'): NodeState => served({ id: 'a', name: 'a', kind: 'asset', inputs: [], partitioned: false, cache: { state: cache, reason: 'no_record', detail: 'reason' }, last_materialization: last ? { status: last, created_at: 'today', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null, error: last === 'failed' ? 'boom' : null } : null, shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, prev_run: null, late_since: null, signals: [] })
describe('graph state', () => {
  it.each(CACHE_STATES)('%s keeps the cache state the server says, and reads as its served cause', (cache) => {
    const node = state(cache)
    const box = graphState(node)
    expect(box).toMatchObject({ kind: 'cache', state: node.cache.state, part: mapPartOf(node.cause), label: mapPartLabel(mapPartOf(node.cause)), lastFailed: false })
    // What the word means, then the server's own account of the node.
    expect(box.hint).toBe(`${mapPartMeaning(mapPartOf(node.cause))} reason`)
  })
  it('a fresh node reads fresh, in the success tone; a node with a cause in the cause\'s word and tone', () => {
    expect(graphState(state('cached'))).toMatchObject({ part: 'fresh', label: 'fresh', tone: 'success', glyph: 'fresh' })
    expect(graphState({ ...state('stale'), cause: 'catching_up' })).toMatchObject({ part: 'catching_up', label: 'catching up', tone: 'running' })
    expect(graphState({ ...state('stale'), cause: 'no_refresh_path' })).toMatchObject({ part: 'no_refresh_path', label: 'no refresh path', tone: 'warning' })
    expect(graphState({ ...state('stale'), cause: 'overdue' })).toMatchObject({ part: 'overdue', label: 'overdue', tone: 'warning' })
    expect(graphState({ ...state('unknown'), cause: 'undecided' })).toMatchObject({ part: 'undecided', label: 'undecided', tone: 'neutral' })
    expect(graphState({ ...state('stale'), cause: 'queued' })).toMatchObject({ part: 'queued', label: 'queued', tone: 'queued' })
  })
  it('a failed latest attempt reads failing; the cache state stays what the server says', () =>
    expect(graphState(state('cached', 'failed'))).toMatchObject({ state: 'cached', part: 'failing', label: 'failing', tone: 'failed', lastFailed: true }))
  it('a node a failure upstream holds reads failing too, as the health map counts it, and names its blocker', () => {
    const held = graphState({ ...state('blocked'), ...blockedBy('p.py:broken'), cause: 'failing' })
    expect(held).toMatchObject({ part: 'failing', label: 'failing', lastFailed: false, blockedBy: 'p.py:broken' })
  })
  it('no state is shown as a snake_case enum', () => {
    for (const cache of CACHE_STATES) expect(graphState(state(cache)).label).not.toMatch(/_/)
  })
})
