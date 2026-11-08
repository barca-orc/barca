/**
 * Every key of a partitioned node (`GET /assets/{id}/partitions/grid`), decoded, filtered and laid
 * out: what the key explorer draws, with no React. A grid's keys are the cells of its dimensions'
 * product in row-major order, the last dimension varying fastest.
 */

import type { Glyph } from './glyph'
import type { GridDimension, PartitionGrid } from './types'

export type KeyState = 'failed' | 'blocked' | 'stale' | 'never_run' | 'fresh'

/** Every key state, most urgent first: the order of the filter chips and the legend. */
export const KEY_STATES: readonly KeyState[] = ['failed', 'blocked', 'stale', 'never_run', 'fresh']

const CODES: Record<string, KeyState> = { c: 'fresh', s: 'stale', n: 'never_run', f: 'failed', b: 'blocked' }

export const KEY_STATE_WORD: Record<KeyState, string> = {
  failed: 'failed',
  blocked: 'blocked',
  stale: 'stale',
  never_run: 'never run',
  fresh: 'fresh',
}

export const KEY_STATE_GLYPH: Record<KeyState, Glyph> = {
  failed: 'failed',
  blocked: 'blocked',
  stale: 'stale',
  never_run: 'never_run',
  fresh: 'fresh',
}

/** What each state means for one key, for a tooltip or the detail. */
export const KEY_STATE_MEANING: Record<KeyState, string> = {
  failed: 'Its latest attempt failed; a run computes it again.',
  blocked: 'It waits on the same key of an input that failed.',
  stale: 'Its code or inputs changed since it succeeded; a run computes it again.',
  never_run: 'It has never run.',
  fresh: 'Served from cache: a successful result matches its code and inputs.',
}

export interface Grid {
  raw: PartitionGrid
  dims: GridDimension[]
  /** Cells: the product of the dimensions' sizes (0 with none). */
  size: number
  /** How far one step of each dimension moves in the cell index. */
  strides: number[]
}

export function gridOf(raw: PartitionGrid): Grid {
  const dims = raw.dimensions
  const strides = dims.map(() => 1)
  for (let d = dims.length - 2; d >= 0; d--) strides[d] = strides[d + 1]! * dims[d + 1]!.values.length
  const size = dims.length === 0 ? 0 : dims.reduce((n, d) => n * d.values.length, 1)
  return { raw, dims, size, strides }
}

export function coordsOf(g: Grid, cell: number): number[] {
  return g.strides.map((stride, d) => Math.floor(cell / stride) % g.dims[d]!.values.length)
}

/** The key of a cell as the server spells it: `day=2026-01-01,region=apac`. */
export function keyOf(g: Grid, cell: number): string {
  return coordsOf(g, cell).map((i, d) => `${g.dims[d]!.name}=${g.dims[d]!.values[i]}`).join(',')
}

/** Each dimension's value of a cell. */
export function valuesOf(g: Grid, cell: number): string[] {
  return coordsOf(g, cell).map((i, d) => g.dims[d]!.values[i]!)
}

/** The cell of a key (its dimensions in any order), or null when the grid has no such key. */
export function cellOf(g: Grid, key: string): number | null {
  const parts = new Map<string, string>()
  for (const pair of key.split(',')) {
    const at = pair.indexOf('=')
    if (at < 0) return null
    parts.set(pair.slice(0, at), pair.slice(at + 1))
  }
  if (parts.size !== g.dims.length) return null
  let cell = 0
  for (let d = 0; d < g.dims.length; d++) {
    const value = parts.get(g.dims[d]!.name)
    const i = value === undefined ? -1 : g.dims[d]!.values.indexOf(value)
    if (i < 0) return null
    cell += i * g.strides[d]!
  }
  return stateOf(g, cell) === null ? null : cell
}

/** A cell's state; null where the cell is not a key. */
export function stateOf(g: Grid, cell: number): KeyState | null {
  return CODES[g.raw.states[cell] ?? '-'] ?? null
}

/** A cell's error, as the grid groups it; null unless the key failed. */
export function errorOf(g: Grid, cell: number): { index: number; text: string } | null {
  const index = g.raw.error[String(cell)]
  if (index === undefined) return null
  return { index, text: g.raw.errors[index]?.error ?? '' }
}

/** A cell's latest attempt; null where there is none. */
export function attemptOf(g: Grid, cell: number): { at: number | null; elapsedMs: number | null; sizeBytes: number | null; run: string | null } | null {
  const at = g.raw.attempted_at[cell] ?? null
  const elapsedMs = g.raw.elapsed_ms[cell] ?? null
  const sizeBytes = g.raw.size_bytes[cell] ?? null
  const run = g.raw.run[cell] ?? null
  if (at === null && elapsedMs === null && sizeBytes === null && run === null) return null
  return {
    at: at === null ? null : g.raw.attempted_base + at,
    elapsedMs,
    sizeBytes,
    run: run === null ? null : g.raw.runs[run] ?? null,
  }
}

/** Which keys the explorer shows: every filter that is set must hold. */
export interface KeyFilter {
  /** Empty: any state. */
  states: readonly KeyState[]
  /** Per dimension, the values kept; a dimension that is absent or empty keeps every value. */
  values: Readonly<Record<string, readonly string[]>>
  /** An index into the grid's errors. */
  error: number | null
  /** A substring of the key, in any case. */
  q: string
}

export const NO_FILTER: KeyFilter = { states: [], values: {}, error: null, q: '' }

export function filterIsSet(f: KeyFilter): boolean {
  return f.states.length > 0 || f.error !== null || f.q.trim() !== '' || Object.values(f.values).some((v) => v.length > 0)
}

/**
 * The keys that match `f`, in key order. Dimension filters narrow by coordinates before any key
 * is spelled, so a filter on one value of a 10,000-key grid reads only its own cells.
 */
export function filterCells(g: Grid, f: KeyFilter): number[] {
  const kept = g.dims.map((d) => {
    const values = f.values[d.name]
    if (!values || values.length === 0) return null
    const set = new Set(values)
    return d.values.map((v) => set.has(v))
  })
  const states = f.states.length > 0 ? new Set(f.states) : null
  const q = f.q.trim().toLowerCase()
  const out: number[] = []
  for (let cell = 0; cell < g.size; cell++) {
    const state = stateOf(g, cell)
    if (state === null) continue
    if (states && !states.has(state)) continue
    if (f.error !== null && g.raw.error[String(cell)] !== f.error) continue
    let ok = true
    for (let d = 0; d < kept.length; d++) {
      const keep = kept[d]
      if (keep && !keep[Math.floor(cell / g.strides[d]!) % g.dims[d]!.values.length]) {
        ok = false
        break
      }
    }
    if (!ok) continue
    if (q && !keyOf(g, cell).toLowerCase().includes(q)) continue
    out.push(cell)
  }
  return out
}

/** How many of `cells` are in each state. */
export function countStates(g: Grid, cells: readonly number[] | null): Record<KeyState, number> {
  const counts: Record<KeyState, number> = { failed: 0, blocked: 0, stale: 0, never_run: 0, fresh: 0 }
  if (cells === null) {
    for (let cell = 0; cell < g.size; cell++) {
      const s = stateOf(g, cell)
      if (s) counts[s]++
    }
  } else {
    for (const cell of cells) {
      const s = stateOf(g, cell)
      if (s) counts[s]++
    }
  }
  return counts
}

/** Keys a run would compute: every state but fresh. */
export function needsRun(state: KeyState | null): boolean {
  return state !== null && state !== 'fresh'
}

/**
 * How the grid is drawn: one block per combination of every dimension but the longest, each a
 * strip of the longest dimension's values in order. A one-dimension grid is one block; day × region
 * is one block per region, each running through the days.
 */
export interface GridBlock {
  /** `region=apac`; empty for a one-dimension grid. */
  label: string
  /** The cell of each value of the long dimension, in order. */
  cells: number[]
}

export interface GridLayout {
  /** The dimension each block runs along. */
  along: number
  blocks: GridBlock[]
}

export function layoutBlocks(g: Grid): GridLayout {
  if (g.dims.length === 0) return { along: 0, blocks: [] }
  let along = 0
  g.dims.forEach((d, i) => { if (d.values.length > g.dims[along]!.values.length) along = i })
  const others = g.dims.map((_, i) => i).filter((i) => i !== along)
  const combos = others.reduce((n, d) => n * g.dims[d]!.values.length, 1)
  const blocks: GridBlock[] = []
  for (let c = 0; c < combos; c++) {
    // The coordinates of this block in the other dimensions, the last varying fastest.
    let rest = c
    const fixed: Array<[number, number]> = []
    for (let k = others.length - 1; k >= 0; k--) {
      const d = others[k]!
      const n = g.dims[d]!.values.length
      fixed.unshift([d, rest % n])
      rest = Math.floor(rest / n)
    }
    const base = fixed.reduce((sum, [d, i]) => sum + i * g.strides[d]!, 0)
    const cells = g.dims[along]!.values.map((_, i) => base + i * g.strides[along]!)
    if (!cells.some((cell) => stateOf(g, cell) !== null)) continue
    blocks.push({ label: fixed.map(([d, i]) => `${g.dims[d]!.name}=${g.dims[d]!.values[i]}`).join(', '), cells })
  }
  return { along, blocks }
}

/** A block and the cell under a canvas point, for hit tests (see `gridGeometry`). */
export interface GridGeometry {
  /** Room at the left for each row's first value (the long dimension's), 0 for none. */
  gutter: number
  cell: number
  gap: number
  columns: number
  /** Each block's top, below its label. */
  tops: number[]
  height: number
  labelHeight: number
}

/** How big each cell is, and where each block starts, for a canvas `width` wide. */
export function gridGeometry(layout: GridLayout, full: number, gutter = 0): GridGeometry {
  const width = Math.max(40, full - gutter)
  const longest = Math.max(1, ...layout.blocks.map((b) => b.cells.length))
  const labelHeight = layout.blocks.length > 1 || layout.blocks[0]?.label ? 20 : 0
  // Big cells for a few keys, small ones for many: at least 4px, at most 18px.
  const cell = Math.max(4, Math.min(18, Math.floor(Math.sqrt((width * 260) / Math.max(1, longest * layout.blocks.length)))))
  const gap = cell >= 8 ? 2 : 1
  const columns = Math.max(1, Math.floor((width + gap) / (cell + gap)))
  const tops: number[] = []
  let y = 0
  for (const block of layout.blocks) {
    y += labelHeight
    tops.push(y)
    y += Math.ceil(block.cells.length / columns) * (cell + gap) + 10
  }
  return { gutter, cell, gap, columns, tops, height: Math.max(0, y - 10), labelHeight }
}

/** The block and position in it of a canvas point, or null where there is no cell. */
export function cellAt(layout: GridLayout, geo: GridGeometry, px: number, y: number): { block: number; index: number } | null {
  const x = px - geo.gutter
  const step = geo.cell + geo.gap
  for (let b = 0; b < layout.blocks.length; b++) {
    const top = geo.tops[b]!
    const rows = Math.ceil(layout.blocks[b]!.cells.length / geo.columns)
    if (y < top || y >= top + rows * step) continue
    const col = Math.floor(x / step)
    const row = Math.floor((y - top) / step)
    if (col < 0 || col >= geo.columns || x - col * step >= geo.cell || y - top - row * step >= geo.cell) return null
    const index = row * geo.columns + col
    return index < layout.blocks[b]!.cells.length ? { block: b, index } : null
  }
  return null
}

/**
 * A range of one block, from one of its positions to another: the keys between them along the
 * block's dimension, with the block's other values. `selection` is what `POST /runs` takes as
 * `partition` to compute just those keys.
 */
export interface KeyRange {
  block: number
  from: number
  to: number
}

export function rangeCells(layout: GridLayout, range: KeyRange): number[] {
  const block = layout.blocks[range.block]
  if (!block) return []
  const [lo, hi] = range.from <= range.to ? [range.from, range.to] : [range.to, range.from]
  return block.cells.slice(lo, hi + 1)
}

/** The `partition` selection of a range: `{"day": [...], "region": "apac"}`. */
export function rangeSelection(g: Grid, layout: GridLayout, range: KeyRange): string | null {
  const cells = rangeCells(layout, range).filter((cell) => stateOf(g, cell) !== null)
  if (cells.length === 0) return null
  const first = coordsOf(g, cells[0]!)
  const selection: Record<string, string | string[]> = {}
  g.dims.forEach((d, i) => {
    selection[d.name] = i === layout.along
      ? cells.map((cell) => d.values[coordsOf(g, cell)[i]!]!)
      : d.values[first[i]!]!
  })
  if (g.dims.length === 1) {
    const values = selection[g.dims[0]!.name] as string[]
    return values.length === 1 ? `${g.dims[0]!.name}=${values[0]}` : JSON.stringify(selection)
  }
  return JSON.stringify(selection)
}

/** The selection of one dimension filter as a run: each filtered dimension's values, every value of the rest. */
export function filterSelection(g: Grid, f: KeyFilter): string | null {
  if (f.states.length > 0 || f.error !== null || f.q.trim() !== '') return null
  const selection: Record<string, string[]> = {}
  let any = false
  for (const d of g.dims) {
    const values = f.values[d.name]
    if (values && values.length > 0) {
      selection[d.name] = [...values]
      any = true
    }
  }
  return any ? JSON.stringify(selection) : null
}
