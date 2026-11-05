import { describe, expect, it } from 'vitest'
import { groupTree } from './groups'
import {
  applyChanges,
  groupedLevel,
  modeChanges,
  nextRowIndex,
  openGroupChanges,
  parseGroupedParams,
  revealChanges,
  rootName,
  searchHits,
  selectChanges,
  visibleFailures,
} from './groupedPage'
import type { ListNode, NodeState } from './types'
import type { Failure } from './health'

const tree = groupTree([
  { id: 'group:p.py:outer', name: 'Outer', description: '', members: ['group:p.py:inner', 'p.py:x'], output: 'p.py:x' },
  { id: 'group:p.py:inner', name: 'Inner', description: '', members: ['p.py:y', 'p.py:check__y'], output: 'p.py:y' },
])
const asset = (id: string, kind: ListNode['kind'] = 'asset', inputs: string[] = []): ListNode =>
  ({ id, kind, freshness: 'always', env: [], file: 'p.py', line: 1, inputs })
const assets = [asset('p.py:x'), asset('p.py:y'), asset('p.py:check__y', 'task', ['p.py:y']), asset('p.py:loose')]
const state = (id: string, over: Partial<NodeState> = {}): NodeState => ({
  id, name: id.split(':')[1]!, kind: id.includes('check__') ? 'task' : 'asset', inputs: [], partitioned: false,
  cache: { state: id.includes('check__') ? 'always_runs' : 'cached', reason: 'materialized', detail: 'd' },
  last_materialization: null, shape: null, env: [], durations: null, next_run: null, last_success_at: null,
  failing_since: null, consecutive_failures: 0, running: null, ...over,
})
const failed = { status: 'failed', created_at: 'today', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, error: 'boom' }
const states = assets.map((a) => state(a.id))
const params = (search: string) => parseGroupedParams(new URLSearchParams(search), tree)

describe('grouped page params', () => {
  it('reads the view, the selection by view, the scope and the search', () => {
    expect(params('pipeline=p.py&view=graph&focus=p.py:y&q=y&sort=name&dir=TB')).toMatchObject({
      pipeline: 'p.py', graph: true, flat: false, show: 'items', selected: 'p.py:y', requestedScope: 'group:p.py:inner', query: 'y', sort: 'name', dir: 'TB',
    })
    expect(params('node=p.py:x&show=failures')).toMatchObject({ graph: false, show: 'failures', selected: 'p.py:x', requestedScope: null })
    expect(params('flat=1&show=failures')).toMatchObject({ flat: true, show: 'items' })
  })

  it('writes each action as URL changes', () => {
    expect(selectChanges(false, 'p.py:x')).toEqual({ node: 'p.py:x' })
    expect(selectChanges(true, null)).toEqual({ focus: null })
    expect(openGroupChanges('group:p.py:inner', null)).toMatchObject({ group: 'group:p.py:inner', node: null, focus: null, q: null, show: null })
    // The sole group is the root: opening it clears the scope; a pipeline card opens its file.
    expect(openGroupChanges('group:p.py:outer', 'group:p.py:outer')).toMatchObject({ group: null })
    expect(openGroupChanges('domain:q.py', null)).toMatchObject({ pipeline: 'q.py', group: null })
    expect(revealChanges('p.py:y', tree, false)).toEqual({ group: 'group:p.py:inner', node: 'p.py:y', focus: null, q: null, show: null })
    expect(revealChanges('p.py:y', tree, true)).toMatchObject({ focus: 'p.py:y', node: null })
    expect(modeChanges('flat', 'group:p.py:inner')).toMatchObject({ flat: '1', group: null })
    expect(modeChanges('failures', 'group:p.py:inner')).toMatchObject({ show: 'failures', group: 'group:p.py:inner', view: null })
    expect(modeChanges('groups', null)).toEqual({ flat: null, show: null, node: null, focus: null })
    expect(applyChanges(new URLSearchParams('a=1&b=2'), { a: null, c: '3' }).toString()).toBe('b=2&c=3')
  })

  it('moves the selection past a heading row and stops at the ends', () => {
    const rows = [null, 'a', 'b', null, 'c']
    expect(nextRowIndex(rows, 'a', 'ArrowDown')).toBe(2)
    expect(nextRowIndex(rows, 'b', 'ArrowDown')).toBe(4)
    expect(nextRowIndex(rows, 'c', 'ArrowDown')).toBeNull()
    expect(nextRowIndex(rows, 'a', 'ArrowUp')).toBeNull()
    expect(nextRowIndex(rows, 'zzz', 'ArrowDown')).toBeNull()
  })

  it('names the root by the pipeline file', () => {
    expect(rootName('pipelines/p.py')).toBe('p.py')
    expect(rootName(null)).toBe('All pipelines')
  })
})

describe('grouped level', () => {
  const input = (search: string, over: Partial<Parameters<typeof groupedLevel>[0]> = {}) =>
    groupedLevel({ params: params(search), tree, allAssets: assets, states, statesFailed: false, live: {}, ...over })

  it('opens the pipeline at its top level, groups and loose nodes together', () => {
    const level = input('pipeline=p.py')
    expect(level.scope).toBeNull()
    expect(level.entries.map((e) => e.id)).toEqual(['group:p.py:outer', 'p.py:loose'])
    expect(level.visualStates['group:p.py:outer']).toMatchObject({ kind: 'group' })
    expect(level.visualStates['p.py:loose']).toMatchObject({ kind: 'cache', state: 'cached' })
  })

  it('opens a group to its members and knows its external inputs', () => {
    const level = input('pipeline=p.py&group=group:p.py:inner')
    expect(level.entries.map((e) => e.id).sort()).toEqual(['p.py:check__y', 'p.py:y'])
    expect(level.scopeIds.sort()).toEqual(['p.py:check__y', 'p.py:y'])
    expect(level.external).toEqual([])
  })

  it('shows Checking… for an entry whose members have no state yet, and the state once it arrives', () => {
    const pending = input('pipeline=p.py', { states: [] })
    expect(pending.visualStates['p.py:loose']).toMatchObject({ kind: 'none', label: 'Checking…' })
    const failedRequest = input('pipeline=p.py', { states: [], statesFailed: true })
    expect(failedRequest.visualStates['p.py:loose']).toMatchObject({ kind: 'none', label: 'State unavailable' })
  })

  it('ranks groups worst first: a failed last attempt before a stale member before a cached one', () => {
    const level = input('pipeline=p.py&group=group:p.py:outer', {
      states: [state('p.py:x'), state('p.py:y'), state('p.py:check__y', { last_materialization: failed })],
    })
    expect(level.visibleEntries.map((e) => e.id)).toEqual(['group:p.py:inner', 'p.py:x'])
    expect(level.visualStates['group:p.py:inner']).toMatchObject({ tone: 'failed', label: '1 last attempt failed' })
  })

  it('a search lists matches below this level with what needs attention first', () => {
    const hits = searchHits({
      needle: 'y', flat: false, show: 'items', graph: false,
      entries: [{ id: 'group:p.py:outer', kind: 'group', inputs: [] }],
      scope: null, leavesOf: new Map(), assets,
      byId: new Map([['p.py:check__y', state('p.py:check__y', { last_materialization: failed })], ['p.py:y', state('p.py:y')]]),
      byKind: new Map(assets.map((a) => [a.id, a.kind])),
    })
    // Every id matches (`.py` has a y): the failed check first, then the rest by name.
    expect(hits.map((h) => h.id)).toEqual(['p.py:check__y', 'p.py:loose', 'p.py:x', 'p.py:y'])
    expect(hits[0]).toMatchObject({ kind: 'task', name: 'check__y' })
    // Nothing in a flat view, the failures list or the graph.
    expect(searchHits({ needle: 'y', flat: true, show: 'items', graph: false, entries: [], scope: null, leavesOf: new Map(), assets, byId: new Map(), byKind: new Map() })).toEqual([])
    const level = input('pipeline=p.py&q=check')
    expect(level.visibleEntries.map((e) => e.id)).toEqual(['group:p.py:outer', 'p.py:check__y'])
    expect(level.isHit.has('p.py:check__y')).toBe(true)
  })

  it('filters failures by id or headline', () => {
    const f = (id: string, headline: string): Failure => ({ id, name: id, kind: 'asset', headline, error: '', at: null, failingSince: null, failures: 1, impact: 0, missingKeys: null })
    const list = [f('p.py:a', 'KeyError'), f('p.py:b', 'ValueError')]
    expect(visibleFailures(list, 'key').map((x) => x.id)).toEqual(['p.py:a'])
    expect(visibleFailures(list, ' ').length).toBe(2)
  })
})
