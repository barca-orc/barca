import { describe, expect, it } from 'vitest'
import { keysHref, keysParams, keysQuery, keysSearch, NO_FILTER, toggleSort, toggleState, withValues } from './keysPage'

describe('the key explorer address', () => {
  it('round-trips a node, a key and every filter', () => {
    const search = 'node=p.py%3Aevents&key=day%3Dd1%2Cregion%3Dapac&state=failed%2Cblocked&v.region=apac%2Cemea&error=0&q=d1&sort=attempted'
    const p = keysParams(new URLSearchParams(search))
    expect(p).toEqual({
      node: 'p.py:events',
      key: 'day=d1,region=apac',
      filter: { states: ['failed', 'blocked'], values: { region: ['apac', 'emea'] }, error: 0, q: 'd1', range: null },
      all: false,
      sort: 'attempted',
    })
    expect(keysSearch(p)).toBe(search)
  })

  it('round-trips a range of the long dimension and the all-keys view', () => {
    const p = keysParams(new URLSearchParams('node=n&range=d1..d9&show=all'))
    expect(p.filter.range).toEqual({ from: 'd1', to: 'd9' })
    expect(p.all).toBe(true)
    expect(keysSearch(p)).toBe('node=n&range=d1..d9&show=all')
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

  it('reads an order it does not know as key order, and a header pressed twice returns to it', () => {
    expect(keysParams(new URLSearchParams('node=a&sort=newest')).sort).toBe('key')
    expect(toggleSort('key', 'size')).toBe('size')
    expect(toggleSort('size', 'size')).toBe('key')
  })
})

describe('what the explorer asks the server', () => {
  const base = { sort: 'key' as const, needsRun: false, along: 'day', rangeValid: true }

  it('spells every filter as the keys route takes it', () => {
    const filter = { states: ['failed' as const, 'blocked' as const], values: { region: ['apac', 'emea'], kind: ['x'] }, error: 1, q: ' 03- ', range: { from: 'd1', to: 'd9' } }
    const query = new URLSearchParams(keysQuery({ ...base, filter, sort: 'elapsed' }))
    expect(query.get('state')).toBe('failed,blocked')
    expect(query.getAll('value')).toEqual(['region=apac,emea', 'kind=x'])
    expect(query.get('range')).toBe('day=d1..d9')
    expect(query.get('error')).toBe('1')
    expect(query.get('q')).toBe('03-')
    expect(query.get('sort')).toBe('elapsed')
    expect(query.has('needs_run')).toBe(false)
  })

  it('asks for the keys a run computes by default, and for every key with no filter', () => {
    expect(keysQuery({ ...base, filter: NO_FILTER, needsRun: true })).toBe('needs_run=1')
    expect(keysQuery({ ...base, filter: NO_FILTER })).toBe('')
  })

  it('leaves out a range the node no longer has, so a stale link shows keys and not an error', () => {
    const filter = { ...NO_FILTER, range: { from: 'd1', to: 'gone' } }
    expect(keysQuery({ ...base, filter, rangeValid: false })).toBe('')
    expect(keysQuery({ ...base, filter, along: null })).toBe('')
  })
})
