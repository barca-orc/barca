import { describe, it, expect } from 'vitest'
import { graphState } from './graphState'
import { CACHE_STATES } from './cacheStateDisplay'
import type { NodeState } from './types'
const state = (cache: NodeState['cache']['state'], failed = false): NodeState => ({ id: 'a', name: 'a', kind: 'asset', inputs: [], partitioned: false, cache: { state: cache, reason: 'no_record', detail: 'reason' }, last_materialization: failed ? { status: 'failed', created_at: 'today', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, error: 'boom' } : null, shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null })
describe('graph state', () => {
  it.each(CACHE_STATES)('%s is shown as its own word, with the server\'s detail', (cache) => {
    expect(graphState(state(cache))).toMatchObject({ kind: 'cache', state: cache, label: cache, hint: 'reason', lastFailed: false })
  })
  it.each([['cached', 'success'], ['stale', 'warning'], ['blocked', 'warning'], ['partial', 'warning'], ['unsynced', 'warning'], ['never_run', 'neutral'], ['unknown', 'neutral'], ['always_runs', 'neutral']] as const)('%s is toned %s', (cache, tone) =>
    expect(graphState(state(cache)).tone).toBe(tone))
  it('a failed latest attempt is a fact beside the state, which stays what the server says', () =>
    expect(graphState(state('cached', true))).toMatchObject({ state: 'cached', label: 'cached', lastFailed: true }))
})
