import { describe, expect, it } from 'vitest'
import {
  attemptOf, cellOf, countStates, errorOf, filterCells, filterSelection, exactSelection, gridOf, keyOf, layoutBlocks, NO_FILTER, stateOf,
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
    counts: { pinned: 0, fresh: 0, stale: 0, never_run: 0, failed: 0, blocked: 0 },
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
    expect(countStates(g, null)).toEqual({ pinned: 0, failed: 1, blocked: 0, stale: 1, never_run: 1, fresh: 2 })
  })

  it('lays out one block per region running through the days', () => {
    const layout = layoutBlocks(g)
    expect(layout.along).toBe(0)
    expect(layout.blocks).toEqual([
      { label: 'region=emea', cells: [0, 2, 4] },
      { label: 'region=apac', cells: [1, 3, 5] },
    ])
  })

  it('turns the dimension filters into a run selection', () => {
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

  it('is one block with no label', () => {
    expect(layoutBlocks(g).blocks).toEqual([{ label: '', cells: [0, 1, 2] }])
  })
})

describe('what is wrong, where', () => {
  // day (6) × region (2): apac fails on d2..d4, emea is blocked on d6.
  const days = ['d1', 'd2', 'd3', 'd4', 'd5', 'd6']
  const states = days.map((_, i) => `${i === 5 ? 'b' : 'c'}${i >= 1 && i <= 3 ? 'f' : 'c'}`).join('')
  const g = gridOf({
    ...dayRegion(states),
    dimensions: [{ name: 'day', values: days }, { name: 'region', values: ['emea', 'apac'] }],
    attempted_at: Array(12).fill(0), elapsed_ms: Array(12).fill(1), size_bytes: Array(12).fill(1), run: Array(12).fill(0),
    error: { '3': 0, '5': 0, '7': 0 },
    errors: [{ error: 'no rows for <region> on <day>', keys: 3 }],
  })

  it('groups failed keys by error, then blocked keys, and says where each sits', async () => {
    const { issueGroups, spreadWords } = await import('./partitionGrid')
    const groups = issueGroups(g)
    expect(groups.map((x) => [x.kind, x.cells.length])).toEqual([['error', 3], ['blocked', 1]])
    const [day, region] = groups[0]!.spread
    expect(spreadWords(region!)).toBe('apac')
    expect(day).toMatchObject({ distinct: 3, from: 'd2', to: 'd4', contiguous: true })
    expect(spreadWords(day!)).toBe('d2, d3, d4')
  })

  it('bins the long dimension into columns, each in its most urgent state, and zooms to a window', async () => {
    const { heatmap, layoutBlocks } = await import('./partitionGrid')
    const layout = layoutBlocks(g)
    const all = heatmap(g, layout, 3)
    expect(all.rows.map((r) => r.label)).toEqual(['emea', 'apac'])
    expect(all.rows[1]!.bins.map((b) => b.worst)).toEqual(['failed', 'failed', 'fresh'])
    expect(all.rows[0]!.bins.map((b) => b.worst)).toEqual(['fresh', 'fresh', 'blocked'])
    expect(all.ticks[0]).toEqual({ bin: 0, label: 'd1' })
    const zoomed = heatmap(g, layout, 10, [1, 3])
    expect(zoomed.rows[1]!.bins.map((b) => b.from)).toEqual([1, 2, 3])
  })

  it('filters and runs a range of the long dimension', async () => {
    const { filterCells, filterSelection, NO_FILTER } = await import('./partitionGrid')
    const f = { ...NO_FILTER, range: { from: 'd4', to: 'd2' }, values: { region: ['apac'] } }
    expect(filterCells(g, f)).toEqual([3, 5, 7])
    expect(JSON.parse(filterSelection(g, f)!)).toEqual({ region: ['apac'], day: ['d2', 'd3', 'd4'] })
  })
})

it('never widens a nonrectangular key selection into additional partitions', () => {
  const g = gridOf(dayRegion('ffffff'))
  expect(JSON.parse(exactSelection(g, [0, 3])!)).toEqual([
    { day: 'd1', region: 'emea' }, { day: 'd2', region: 'apac' },
  ])
  expect(exactSelection(g, [])).toBeNull()
  expect(JSON.parse(exactSelection(g, [0, 1])!)).toEqual({ day: ['d1'], region: ['emea', 'apac'] })
})
