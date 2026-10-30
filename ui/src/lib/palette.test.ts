import { describe, expect, it } from 'vitest'
import { INITIAL_PALETTE, paletteActions, paletteIndex, searchPalette, updatePalette } from './palette'

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
})

describe('searchPalette', () => {
  it('ranks a name prefix over a name match over a detail match', () => {
    expect(searchPalette(index, 'inv').map((i) => i.label)).toEqual(['invoices', 'check__invoices'])
    expect(searchPalette(index, 'crm').map((i) => i.key)).toEqual(['file:crm.py', 'crm.py:poll'])
  })
  it('caps the list', () => {
    expect(searchPalette(index, '', 2)).toHaveLength(2)
    expect(searchPalette(index, 'zzz')).toEqual([])
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
