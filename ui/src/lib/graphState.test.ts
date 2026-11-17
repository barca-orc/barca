import { describe, it, expect } from 'vitest'
import { graphState } from './graphState'
import { CACHE_STATES, cacheStateLabel } from './cacheStateDisplay'
import type { NodeState } from './types'
const state = (cache: NodeState['cache']['state'], last: 'failed' | 'success' | null = 'success'): NodeState => ({ id: 'a', name: 'a', kind: 'asset', inputs: [], partitioned: false, cache: { state: cache, reason: 'no_record', detail: 'reason' }, last_materialization: last ? { status: last, created_at: 'today', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null, error: last === 'failed' ? 'boom' : null } : null, shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, prev_run: null, late_since: null, signals: [] })
describe('graph state', () => {
  it.each(CACHE_STATES)('%s is shown in plain words, with the server\'s detail', (cache) => {
    expect(graphState(state(cache))).toMatchObject({ kind: 'cache', state: cache, label: cacheStateLabel(cache), hint: 'reason', lastFailed: false })
  })
  it.each([['cached', 'success'], ['stale', 'warning'], ['blocked', 'warning'], ['partial', 'warning'], ['unsynced', 'warning'], ['never_run', 'neutral'], ['unknown', 'neutral'], ['always_runs', 'success']] as const)('%s is toned %s', (cache, tone) =>
    expect(graphState(state(cache)).tone).toBe(tone))
  it('a failed latest attempt is the one state shown, failing; the cache state stays what the server says', () =>
    expect(graphState(state('cached', 'failed'))).toMatchObject({ state: 'cached', label: 'failing', tone: 'failed', lastFailed: true }))
  it('a node that always runs shows its last run: succeeded, failing, or never run', () => {
    expect(graphState(state('always_runs'))).toMatchObject({ state: 'always_runs', label: 'succeeded', glyph: 'fresh' })
    expect(graphState(state('always_runs', 'failed'))).toMatchObject({ label: 'failing', lastFailed: true })
    expect(graphState(state('always_runs', null))).toMatchObject({ state: 'never_run', label: 'never run', glyph: 'never_run' })
  })
  it('no state is shown as a snake_case enum', () => {
    for (const cache of CACHE_STATES) expect(graphState(state(cache)).label).not.toMatch(/_/)
  })
})
