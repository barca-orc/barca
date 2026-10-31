import { describe, expect, it } from 'vitest'
import { INITIAL_PALETTE, isKeyQuery, keyItems, paletteActions, paletteIndex, searchPalette, updatePalette } from './palette'

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

const row = (key: string, status: string, created_at = '2026-10-30 00:00:00') => ({ key, status, created_at, elapsed_seconds: null, error: null, run_id: null, artifact_path: null, size_bytes: null })

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
  it('opens the node panel with the key searched', () => {
    expect(items[0]?.target).toEqual({ type: 'go', to: '/assets?pipeline=planning.py&node=planning.py%3Ademand_forecast&key=ra-02541' })
  })
  it('keeps a key of several dimensions whole in the search', () => {
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
