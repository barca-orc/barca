import { describe, expect, it } from 'vitest'
import { alongValues, cellKey, heatmap, rangeBounds } from './partitionGrid'
import { KEY_STATES, keyStateDisplay, keyStateOf, keyStateOfCode } from './keyStateDisplay'
import type { KeysOverview } from './types'

/** day (6) × region (2), as the server rows it: apac fails on d2..d4, emea is blocked on d6, apac has no d6. */
function overview(extra: Partial<KeysOverview> = {}): KeysOverview {
  return {
    id: 'p.py:events',
    chain: ['p.py:events'],
    known: true,
    dimensions: [
      { name: 'day', values: ['d1', 'd2', 'd3', 'd4', 'd5', 'd6'] },
      { name: 'region', values: ['emea', 'apac'] },
    ],
    counts: { fresh: 7, pinned: 0, stale: 0, never_run: 0, failed: 3, blocked: 1, empty: 0 },
    needs_run: 4,
    errors: [{ error: 'no rows for <region> on <day>', keys: 3 }],
    issues: [],
    along: 'day',
    rows: [
      { label: 'region=emea', values: ['emea'], states: 'cccccb' },
      { label: 'region=apac', values: ['apac'], states: 'cfffc-' },
    ],
    rows_total: 2,
    truncated: false,
    last_attempt_at: null,
    ...extra,
  }
}

describe('the overview drawn from served rows', () => {
  it('reads the values of the dimension the rows run along', () => {
    expect(alongValues(overview())).toEqual(['d1', 'd2', 'd3', 'd4', 'd5', 'd6'])
    expect(alongValues(overview({ along: null }))).toEqual([])
  })

  it('bins the long dimension into columns, each in its most urgent state', () => {
    const all = heatmap(overview(), 3)
    expect(all.rows.map((r) => r.label)).toEqual(['emea', 'apac'])
    expect(all.rows[1]!.bins.map((b) => b.worst)).toEqual(['failed', 'failed', 'fresh'])
    expect(all.rows[0]!.bins.map((b) => b.worst)).toEqual(['fresh', 'fresh', 'blocked'])
    expect(all.rows[1]!.bins[1]!.counts).toMatchObject({ failed: 2, fresh: 0 })
    // A gap is not a key: the last apac column counts one key, not two.
    expect(KEY_STATES.reduce((n, s) => n + all.rows[1]!.bins[2]!.counts[s], 0)).toBe(1)
    expect(all.ticks[0]).toEqual({ bin: 0, label: 'd1' })
  })

  it('zooms to a window, down to one value per column', () => {
    const zoomed = heatmap(overview(), 10, [1, 3])
    expect(zoomed.window).toEqual([1, 3])
    expect(zoomed.rows[1]!.bins.map((b) => [b.from, b.to])).toEqual([[1, 1], [2, 2], [3, 3]])
    expect(zoomed.rows[1]!.bins.map((b) => b.worst)).toEqual(['failed', 'failed', 'failed'])
  })

  it('reads a range either way round, and none for a value the dimension lacks', () => {
    const days = alongValues(overview())
    expect(rangeBounds(days, { from: 'd4', to: 'd2' })).toEqual([1, 3])
    expect(rangeBounds(days, { from: 'd4', to: 'gone' })).toBeNull()
    expect(rangeBounds(days, null)).toBeNull()
  })

  it('spells the key a cell stands for as the server does, the dimensions in their order', () => {
    const o = overview()
    expect(cellKey(o, o.rows[1]!, 2)).toBe('day=d3,region=apac')
    const one = overview({ dimensions: [{ name: 'k', values: ['a', 'b'] }], along: 'k', rows: [{ label: '', values: [], states: 'cf' }] })
    expect(cellKey(one, one.rows[0]!, 1)).toBe('k=b')
    expect(heatmap(one, 8).rows[0]!.label).toBe('')
  })
})

describe('how a key state is shown', () => {
  it('reads the characters of a served row, and nothing for a gap or an unknown one', () => {
    expect('fbsnpc-x'.split('').map((c) => keyStateOfCode(c))).toEqual(['failed', 'blocked', 'stale', 'never_run', 'pinned', 'fresh', null, null])
    expect(keyStateOfCode(undefined)).toBeNull()
  })

  it('has a word and a glyph for every state, and reads a served name', () => {
    expect(KEY_STATES.map((s) => keyStateDisplay(s).word)).toEqual(['failed', 'blocked', 'stale', 'never run', 'pinned', 'fresh'])
    expect(keyStateOf('never_run')).toBe('never_run')
    expect(keyStateOf('cached')).toBeNull()
  })
})
