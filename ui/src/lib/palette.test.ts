import { describe, expect, it } from 'vitest'
import type { GroupScope } from './groups'
import { groupScope } from './testdata/rollup'
import type { NodeState } from './types'
import { INITIAL_PALETTE, isKeyQuery, keyItems, paletteActions, paletteIndex, searchPalette, updatePalette } from './palette'
import { served } from './testdata/served'

const index = paletteIndex(
  [{ id: 'billing.py:invoices', kind: 'asset' }, { id: 'billing.py:check__invoices', kind: 'task' }, { id: 'crm.py:poll', kind: 'sensor' }],
  [{ id: 'group:billing.py:raw', name: 'Raw' }],
  [{ id: 'run-1', target: 'billing.py', status: 'failed' }],
)

describe('paletteIndex', () => {
  it('links each kind to where it opens', () => {
    const to = (key: string) => index.find((i) => i.key === key)?.target
    expect(to('billing.py:invoices')).toEqual({ type: 'go', to: '/assets?pipeline=billing.py&node=billing.py%3Ainvoices' })
    expect(to('billing.py:check__invoices')).toMatchObject({ to: expect.stringContaining('/tasks?') })
    expect(to('crm.py:poll')).toMatchObject({ to: expect.stringContaining('/sensors?') })
    expect(to('group:billing.py:raw')).toEqual({ type: 'go', to: '/assets?pipeline=billing.py&group=group%3Abilling.py%3Araw' })
    expect(to('run:run-1')).toEqual({ type: 'go', to: '/runs?run=run-1' })
  })
  it('lists what the project is made of before its runs', () => {
    expect(index.map((i) => i.section)).toEqual(['Files', 'Files', 'Groups', 'Nodes', 'Nodes', 'Nodes', 'Runs'])
  })
})

describe('a result\'s state', () => {
  const node = (id: string, failed: boolean) => served({
    id, name: id, kind: 'asset', inputs: [], partitioned: false, cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: { status: failed ? 'failed' : 'success', created_at: '2026-10-30 00:00:00', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null },
    shape: null, env: [], durations: null, next_run: null, running: null, last_success_at: null, failing_since: null, consecutive_failures: 0, prev_run: null, late_since: null, signals: [],
  }) as NodeState
  const assets = [{ id: 'a.py:orders', kind: 'asset' as const }, { id: 'a.py:orders_daily', kind: 'asset' as const }, { id: 'b.py:orders_bad', kind: 'asset' as const }]
  const states = {
    byId: new Map([['a.py:orders', node('a.py:orders', false)], ['a.py:orders_daily', node('a.py:orders_daily', false)], ['b.py:orders_bad', node('b.py:orders_bad', true)]]),
    // The project's rollup entries: the group's, and the file of the nodes in no group.
    entries: new Map<string, GroupScope>([['group:a.py:raw', groupScope({}, 2)], ['file:b.py', groupScope({ failing: 1 })]]),
  }
  const withStates = paletteIndex(assets, [{ id: 'group:a.py:raw', name: 'Raw' }], [{ id: 'run-9', target: null, status: 'failed' }], states)
  const of = (key: string) => withStates.find((i) => i.key === key)?.state

  it('draws each node, group, file and run with its glyph', () => {
    expect(of('a.py:orders')).toMatchObject({ glyph: 'fresh', label: 'fresh' })
    expect(of('b.py:orders_bad')).toMatchObject({ glyph: 'failed', label: 'failing', tone: 'failed' })
    expect(of('group:a.py:raw')).toMatchObject({ glyph: 'fresh' })
    expect(of('file:b.py')).toMatchObject({ glyph: 'failed', label: '1 failing' })
    expect(of('run:run-9')).toMatchObject({ glyph: 'failed', label: 'failed' })
  })
  it('has none for what is not loaded yet', () => {
    expect(index.every((i) => i.section === 'Runs' || i.state === null)).toBe(true)
  })
  it('ranks failing results first among equal matches, never above a better match', () => {
    // Three name-prefix matches: the failing one first, then index order.
    expect(searchPalette(withStates, 'orders').filter((i) => i.section === 'Nodes').map((i) => i.key)).toEqual(['b.py:orders_bad', 'a.py:orders', 'a.py:orders_daily'])
    // `daily` is a name match for one node only: a failing detail match does not outrank it.
    expect(searchPalette(withStates, 'daily').map((i) => i.key)[0]).toBe('a.py:orders_daily')
  })
})

describe('searchPalette', () => {
  it('ranks a name prefix over a name match over a detail match', () => {
    expect(searchPalette(index, 'inv').map((i) => i.label)).toEqual(['invoices', 'check__invoices'])
    expect(searchPalette(index, 'crm').map((i) => i.key)).toEqual(['file:crm.py', 'crm.py:poll'])
  })
  it('reaches a run by its id prefix', () => {
    expect(searchPalette(index, 'run-').map((i) => i.key)).toEqual(['run:run-1'])
  })
  it('caps the list', () => {
    expect(searchPalette(index, '', [], 2)).toHaveLength(2)
    expect(searchPalette(index, 'zzz')).toEqual([])
  })
  it('appends the keys found after the index matches, within the cap', () => {
    const keys = keyItems([{ node: 'billing.py:invoices', keys: [row('account=inv-1', 'cached')] }], 0)
    expect(searchPalette(index, 'inv', keys).map((i) => i.key)).toEqual(['billing.py:invoices', 'billing.py:check__invoices', 'key:billing.py:invoices:account=inv-1'])
    expect(searchPalette(index, 'inv-1', keys).map((i) => i.key)).toEqual(['key:billing.py:invoices:account=inv-1'])
    expect(searchPalette(index, 'inv', keys, 2)).toHaveLength(2)
  })
})

const row = (key: string, status: string, created_at = '2026-10-30 00:00:00') => ({ key, status, created_at, elapsed_seconds: null, error: null, run_id: null, artifact_path: null, size_bytes: null, rows: null })

describe('isKeyQuery', () => {
  it('is anything with a digit, =, -, _ or /, of two characters or more', () => {
    for (const q of ['ra-02541', 'account=ra', '2026-10-30', 'us_east', 'eu/west', ' 42 ']) expect(isKeyQuery(q)).toBe(true)
    for (const q of ['invoices', 'r', '-', '']) expect(isKeyQuery(q)).toBe(false)
  })
})

describe('keyItems', () => {
  const now = Date.parse('2026-10-30T00:02:00Z')
  const items = keyItems(
    [
      { node: 'planning.py:demand_forecast', keys: [row('account=ra-02540', 'cached'), row('account=ra-02541', 'failed')] },
      { node: 'planning.py:promo_plan', keys: [row('account=ra-02541', 'failed'), row('account=ra-02542', 'cached')] },
    ],
    now,
  )
  it('names the node and the key, with the state and its age', () => {
    expect(items.map((i) => [i.label, i.detail])).toEqual([
      ['demand_forecast · account=ra-02541', 'failed 2m ago'],
      ['promo_plan · account=ra-02541', 'failed 2m ago'],
      ['demand_forecast · account=ra-02540', 'cached 2m ago'],
      ['promo_plan · account=ra-02542', 'cached 2m ago'],
    ])
    expect(items.every((i) => i.section === 'Keys')).toBe(true)
  })
  it('opens the key explorer at the key', () => {
    expect(items[0]?.target).toEqual({ type: 'go', to: '/keys?node=planning.py%3Ademand_forecast&key=account%3Dra-02541' })
  })
  it('keeps a key of several dimensions whole', () => {
    const [item] = keyItems([{ node: 'a.py:n', keys: [row('region=eu,day=2026-10-30', 'cached')] }], now)
    expect(item?.target).toMatchObject({ to: expect.stringContaining('key=region%3Deu%2Cday%3D2026-10-30') })
  })
})

describe('paletteActions', () => {
  it('offers what the page can do', () => {
    expect(paletteActions('Run a.py (1 asset)', true).map((a) => a.target.type)).toEqual(['run', 'open_graph'])
    expect(paletteActions(null, false)).toEqual([])
  })
})

describe('updatePalette', () => {
  it('wraps the highlight, and a new query resets it', () => {
    const [up] = updatePalette(INITIAL_PALETTE, { type: 'moved', by: -1, count: 3 })
    expect(up.active).toBe(2)
    expect(updatePalette(up, { type: 'moved', by: 1, count: 3 })[0].active).toBe(0)
    expect(updatePalette(up, { type: 'query_changed', value: 'a' })[0]).toEqual({ query: 'a', active: 0 })
  })
})
