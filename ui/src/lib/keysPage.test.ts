import { describe, expect, it } from 'vitest'
import { keysHref, keysParams, keysSearch, toggleState, withValues } from './keysPage'
import { NO_FILTER } from './partitionGrid'

describe('the key explorer address', () => {
  it('round-trips a node, a key and every filter', () => {
    const search = 'node=p.py%3Aevents&key=day%3Dd1%2Cregion%3Dapac&state=failed%2Cblocked&v.region=apac%2Cemea&error=0&q=d1'
    const p = keysParams(new URLSearchParams(search))
    expect(p).toEqual({
      node: 'p.py:events',
      key: 'day=d1,region=apac',
      filter: { states: ['failed', 'blocked'], values: { region: ['apac', 'emea'] }, error: 0, q: 'd1' },
    })
    expect(keysSearch(p)).toBe(search)
  })

  it('ignores states it does not know and a malformed error index', () => {
    const p = keysParams(new URLSearchParams('node=a&state=failed,bogus&error=-1'))
    expect(p.filter.states).toEqual(['failed'])
    expect(p.filter.error).toBeNull()
  })

  it('links to a node with a filter', () => {
    expect(keysHref('p.py:events', { filter: { states: ['failed'] } })).toBe('/keys?node=p.py%3Aevents&state=failed')
  })

  it('toggles states in the chips order and clears a dimension with no values', () => {
    const f = toggleState(toggleState(NO_FILTER, 'fresh'), 'failed')
    expect(f.states).toEqual(['failed', 'fresh'])
    expect(toggleState(f, 'fresh').states).toEqual(['failed'])
    expect(withValues(withValues(NO_FILTER, 'region', ['apac']), 'region', []).values).toEqual({})
  })
})
