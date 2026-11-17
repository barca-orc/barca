import { describe, expect, it } from 'vitest'
import { knownStates } from './knownStates'
import type { NodeState } from './types'
import { served } from './testdata/served'

const state = (id: string, cache: NodeState['cache']['state']): NodeState => served({
  id, name: id, kind: 'asset', inputs: [], partitioned: false,
  cache: { state: cache, reason: 'materialized', detail: 'd' },
  last_materialization: null, shape: null, env: [], durations: null, next_run: null, last_success_at: null,
  failing_since: null, consecutive_failures: 0, running: null, prev_run: null, late_since: null, signals: [],
})

describe('knownStates', () => {
  it('is the scope\'s own rows once they have arrived', () => {
    const own = [state('a', 'cached')]
    expect(knownStates(own, [{ data: [state('a', 'stale')], updatedAt: 9 }])).toBe(own)
  })
  it('keeps each node\'s newest known row while the scope loads, instead of nothing', () => {
    const out = knownStates(undefined, [
      { data: [state('a', 'stale'), state('b', 'cached')], updatedAt: 1 },
      { data: [state('a', 'cached')], updatedAt: 2 },
      { data: undefined, updatedAt: 3 },
    ])
    expect(out?.map((s) => [s.id, s.cache.state])).toEqual([['a', 'cached'], ['b', 'cached']])
  })
  it('is undefined when nothing has ever loaded: the one time a loading state is shown', () => {
    expect(knownStates(undefined, [])).toBeUndefined()
    expect(knownStates(undefined, [{ data: [], updatedAt: 1 }])).toBeUndefined()
  })
})
