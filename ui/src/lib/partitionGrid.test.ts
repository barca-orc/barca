import { describe, expect, it } from 'vitest'
import {
  attemptOf, cellAt, cellOf, countStates, errorOf, filterCells, filterSelection, gridGeometry, gridOf, keyOf,
  layoutBlocks, NO_FILTER, rangeSelection, stateOf,
} from './partitionGrid'
import type { PartitionGrid } from './types'

/** day (3) × region (2): cell = day * 2 + region. */
function dayRegion(states: string, extra: Partial<PartitionGrid> = {}): PartitionGrid {
  return {
    id: 'p.py:events',
    chain: ['p.py:events'],
    known: true,
    dimensions: [
      { name: 'day', values: ['d1', 'd2', 'd3'] },
      { name: 'region', values: ['emea', 'apac'] },
    ],
    states,
    counts: { fresh: 0, stale: 0, never_run: 0, failed: 0, blocked: 0 },
    attempted_base: 1000,
    attempted_at: [0, 0, 5, 5, null, null],
    elapsed_ms: [10, 12, 9, 40, null, null],
    size_bytes: [100, 100, 90, null, null, null],
    run: [0, 0, 1, 1, null, null],
    error: { '3': 0 },
    errors: [{ error: 'no rows for <region> on <day>', keys: 1 }],
    runs: ['r1', 'r2'],
    ...extra,
  }
}

describe('a grid', () => {
  const g = gridOf(dayRegion('ccsfn-'))

  it('spells each cell as the server does, the last dimension varying fastest', () => {
    expect(g.size).toBe(6)
    expect(keyOf(g, 0)).toBe('day=d1,region=emea')
    expect(keyOf(g, 3)).toBe('day=d2,region=apac')
    expect(cellOf(g, 'region=apac,day=d2')).toBe(3)
    expect(cellOf(g, 'day=d3,region=apac')).toBeNull() // not a key
    expect(cellOf(g, 'day=d9,region=apac')).toBeNull()
  })

  it('reads states, errors and attempts per cell', () => {
    expect([0, 1, 2, 3, 4, 5].map((c) => stateOf(g, c))).toEqual(['fresh', 'fresh', 'stale', 'failed', 'never_run', null])
    expect(errorOf(g, 3)).toEqual({ index: 0, text: 'no rows for <region> on <day>' })
    expect(errorOf(g, 2)).toBeNull()
    expect(attemptOf(g, 3)).toEqual({ at: 1005, elapsedMs: 40, sizeBytes: null, run: 'r2' })
    expect(attemptOf(g, 4)).toBeNull()
  })

  it('filters by state, value, error and text, leaving out cells that are not keys', () => {
    expect(filterCells(g, NO_FILTER)).toEqual([0, 1, 2, 3, 4])
    expect(filterCells(g, { ...NO_FILTER, states: ['failed', 'never_run'] })).toEqual([3, 4])
    expect(filterCells(g, { ...NO_FILTER, values: { region: ['apac'] } })).toEqual([1, 3])
    expect(filterCells(g, { ...NO_FILTER, error: 0 })).toEqual([3])
    expect(filterCells(g, { ...NO_FILTER, q: 'D2' })).toEqual([2, 3])
    expect(countStates(g, null)).toEqual({ failed: 1, blocked: 0, stale: 1, never_run: 1, fresh: 2 })
  })

  it('lays out one block per region running through the days', () => {
    const layout = layoutBlocks(g)
    expect(layout.along).toBe(0)
    expect(layout.blocks).toEqual([
      { label: 'region=emea', cells: [0, 2, 4] },
      { label: 'region=apac', cells: [1, 3, 5] },
    ])
  })

  it('turns a range of one block into a run selection', () => {
    const layout = layoutBlocks(g)
    expect(JSON.parse(rangeSelection(g, layout, { block: 1, from: 1, to: 0 })!)).toEqual({ day: ['d1', 'd2'], region: 'apac' })
    // d3 × apac is not a key: the range keeps the keys it covers.
    expect(JSON.parse(rangeSelection(g, layout, { block: 1, from: 1, to: 2 })!)).toEqual({ day: ['d2'], region: 'apac' })
    expect(JSON.parse(filterSelection(g, { ...NO_FILTER, values: { region: ['apac'] } })!)).toEqual({ region: ['apac'] })
    expect(filterSelection(g, { ...NO_FILTER, states: ['failed'] })).toBeNull()
  })
})

describe('a one-dimension grid', () => {
  const g = gridOf({
    ...dayRegion('cfc'),
    dimensions: [{ name: 'k', values: ['a', 'b', 'c'] }],
    attempted_at: [0, 0, 0], elapsed_ms: [1, 1, 1], size_bytes: [1, 1, 1], run: [0, 0, 0], error: { '1': 0 },
  })

  it('is one block with no label, and a single key runs as key=value', () => {
    const layout = layoutBlocks(g)
    expect(layout.blocks).toEqual([{ label: '', cells: [0, 1, 2] }])
    expect(rangeSelection(g, layout, { block: 0, from: 1, to: 1 })).toBe('k=b')
  })

  it('maps canvas points to cells and misses the gaps', () => {
    const layout = layoutBlocks(g)
    const geo = gridGeometry(layout, 100)
    expect(geo.labelHeight).toBe(0)
    const step = geo.cell + geo.gap
    expect(cellAt(layout, geo, 1, 1)).toEqual({ block: 0, index: 0 })
    expect(cellAt(layout, geo, step + 1, 1)).toEqual({ block: 0, index: 1 })
    expect(cellAt(layout, geo, geo.cell + 0.5, 1)).toBeNull() // the gap
    expect(cellAt(layout, geo, 3 * step + 1, 1)).toBeNull() // past the last key
  })
})
