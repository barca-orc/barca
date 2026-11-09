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
  /** A stretch of the longest dimension (its values, in order, from and to inclusive). */
  range: { from: string; to: string } | null
}

export const NO_FILTER: KeyFilter = { states: [], values: {}, error: null, q: '', range: null }

/** The dimension a grid's blocks and its overview run along: the one with the most values. */
export function alongOf(g: Grid): number {
  let along = 0
  g.dims.forEach((d, i) => { if (d.values.length > g.dims[along]!.values.length) along = i })
  return along
}

/** A range's index bounds along `alongOf(g)`, or null when it names no value of it. */
export function rangeBounds(g: Grid, range: KeyFilter['range']): [number, number] | null {
  if (!range || g.dims.length === 0) return null
  const values = g.dims[alongOf(g)]!.values
  const a = values.indexOf(range.from)
  const b = values.indexOf(range.to)
  if (a < 0 || b < 0) return null
  return a <= b ? [a, b] : [b, a]
}

export function filterIsSet(f: KeyFilter): boolean {
  return f.states.length > 0 || f.error !== null || f.q.trim() !== '' || f.range !== null || Object.values(f.values).some((v) => v.length > 0)
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
  const bounds = rangeBounds(g, f.range)
  const along = alongOf(g)
  const out: number[] = []
  for (let cell = 0; cell < g.size; cell++) {
    const state = stateOf(g, cell)
    if (state === null) continue
    if (states && !states.has(state)) continue
    if (bounds) {
      const i = Math.floor(cell / g.strides[along]!) % g.dims[along]!.values.length
      if (i < bounds[0] || i > bounds[1]) continue
    }
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

/** The run selection of the dimension filters and the range: each filtered dimension's values, every value of the rest. */
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
  const bounds = rangeBounds(g, f.range)
  if (bounds) {
    const d = g.dims[alongOf(g)]!
    const inRange = d.values.slice(bounds[0], bounds[1] + 1)
    const kept = selection[d.name]
    selection[d.name] = kept ? inRange.filter((v) => kept.includes(v)) : inRange
    any = true
  }
  return any ? JSON.stringify(selection) : null
}

// ── What is wrong, where ──────────────────────────────────────────────────────

/** How one group of keys spreads over one dimension: which values, how many keys each. */
export interface DimSpread {
  name: string
  /** Distinct values among the group's keys. */
  distinct: number
  /** Of how many the dimension has. */
  of: number
  /** The values with the most keys, at most three, most first. */
  top: Array<{ value: string; keys: number }>
  /** The first and last value in the dimension's order. */
  from: string
  to: string
  /** The values are one unbroken stretch of the dimension. */
  contiguous: boolean
}

/** A problem shared by some keys: one error of the failed keys, or a state (blocked, stale, never run). */
export interface IssueGroup {
  kind: 'error' | 'blocked' | 'stale' | 'never_run'
  /** The error's index, for an error group. */
  error: number | null
  /** The error's first line, or what the state means. */
  title: string
  cells: number[]
  spread: DimSpread[]
}

function spreadOf(g: Grid, cells: readonly number[]): DimSpread[] {
  return g.dims.map((d, i) => {
    const counts = new Map<number, number>()
    for (const cell of cells) {
      const v = Math.floor(cell / g.strides[i]!) % d.values.length
      counts.set(v, (counts.get(v) ?? 0) + 1)
    }
    const idx = [...counts.keys()].sort((a, b) => a - b)
    const top = [...counts.entries()].sort((a, b) => b[1] - a[1] || a[0] - b[0]).slice(0, 3).map(([v, keys]) => ({ value: d.values[v]!, keys }))
    return {
      name: d.name,
      distinct: idx.length,
      of: d.values.length,
      top,
      from: d.values[idx[0] ?? 0] ?? '',
      to: d.values[idx[idx.length - 1] ?? 0] ?? '',
      contiguous: idx.length > 0 && idx[idx.length - 1]! - idx[0]! === idx.length - 1,
    }
  })
}

const STATE_TITLE: Record<'blocked' | 'stale' | 'never_run', string> = {
  blocked: 'Waiting on the same keys of an input that failed',
  stale: 'Code or inputs changed since they succeeded',
  never_run: 'Never run',
}

/** Every problem of a grid, the most keys first: each error of its failed keys, then blocked, stale and never-run keys. */
export function issueGroups(g: Grid): IssueGroup[] {
  const byError = new Map<number, number[]>()
  const byState: Record<'blocked' | 'stale' | 'never_run', number[]> = { blocked: [], stale: [], never_run: [] }
  for (let cell = 0; cell < g.size; cell++) {
    const state = stateOf(g, cell)
    if (state === 'failed') {
      const e = g.raw.error[String(cell)] ?? -1
      const list = byError.get(e) ?? []
      list.push(cell)
      byError.set(e, list)
    } else if (state === 'blocked' || state === 'stale' || state === 'never_run') {
      byState[state].push(cell)
    }
  }
  const groups: IssueGroup[] = [...byError.entries()].map(([e, cells]) => ({
    kind: 'error' as const,
    error: e < 0 ? null : e,
    title: e < 0 ? 'Failed' : g.raw.errors[e]?.error ?? 'Failed',
    cells,
    spread: spreadOf(g, cells),
  }))
  groups.sort((a, b) => b.cells.length - a.cells.length)
  for (const state of ['blocked', 'stale', 'never_run'] as const) {
    if (byState[state].length > 0) groups.push({ kind: state, error: null, title: STATE_TITLE[state], cells: byState[state], spread: spreadOf(g, byState[state]) })
  }
  return groups
}

/** Where a group sits, in words, for each dimension it does not span whole: `apac`, `2023-03-01 → 2023-03-20`, `7 values`. */
export function spreadWords(s: DimSpread): string | null {
  if (s.distinct === s.of && s.of > 1) return null
  if (s.distinct <= 3) return s.top.map((t) => t.value).join(', ')
  if (s.contiguous) return `${s.from} → ${s.to}`
  return `${s.distinct.toLocaleString()} values, ${s.from} … ${s.to}`
}

// ── The overview: every key in one small picture ──────────────────────────────

/** One column of the overview: a stretch of the long dimension in one row. */
export interface HeatBin {
  /** Index bounds along the long dimension, inclusive. */
  from: number
  to: number
  counts: Record<KeyState, number>
  /** The most urgent state in it; null where it has no key. */
  worst: KeyState | null
}

export interface Heatmap {
  along: number
  /** One row per value combination of the other dimensions; one row for a one-dimension grid. */
  rows: Array<{ label: string; bins: HeatBin[] }>
  /** Labels under the columns: the long dimension's value where each starts, a few of them. */
  ticks: Array<{ bin: number; label: string }>
  /** The stretch drawn, inclusive. */
  window: [number, number]
}

/**
 * Every key in at most `columns` columns: each row a block of `layoutBlocks`, its long dimension
 * cut into equal stretches, each drawn in the most urgent state of its keys. Keys a filter leaves
 * out still count; `window` zooms to a stretch of the long dimension.
 */
export function heatmap(g: Grid, layout: GridLayout, columns: number, window: [number, number] | null = null, maxTicks = 6): Heatmap {
  const along = layout.along
  const values = g.dims[along]?.values ?? []
  const [lo, hi] = window ?? [0, Math.max(0, values.length - 1)]
  const n = hi - lo + 1
  const cols = Math.max(1, Math.min(columns, n))
  const edges = Array.from({ length: cols + 1 }, (_, b) => lo + Math.floor((b * n) / cols))
  const rows = layout.blocks.map((block) => ({
    label: block.label.replace(/[^=, ]+=/g, ''),
    bins: Array.from({ length: cols }, (_, b): HeatBin => {
      const counts: Record<KeyState, number> = { failed: 0, blocked: 0, stale: 0, never_run: 0, fresh: 0 }
      for (let i = edges[b]!; i < edges[b + 1]!; i++) {
        const s = stateOf(g, block.cells[i]!)
        if (s) counts[s]++
      }
      const worst = KEY_STATES.find((s) => counts[s] > 0) ?? null
      return { from: edges[b]!, to: edges[b + 1]! - 1, counts, worst }
    }),
  }))
  const tickCount = Math.max(1, Math.min(cols, maxTicks))
  const ticks = Array.from({ length: tickCount }, (_, t) => {
    const bin = Math.round((t * (cols - 1)) / Math.max(1, tickCount - 1))
    return { bin, label: values[edges[bin]!] ?? '' }
  })
  return { along, rows, ticks, window: [lo, hi] }
}

/** The run selection that covers an issue's keys: each dimension's values among them (with a few more keys, served from cache when fresh). */
export function issueSelection(g: Grid, issue: IssueGroup): string {
  const selection: Record<string, string[]> = {}
  g.dims.forEach((d, i) => {
    const seen = new Set<number>()
    for (const cell of issue.cells) seen.add(Math.floor(cell / g.strides[i]!) % d.values.length)
    selection[d.name] = [...seen].sort((a, b) => a - b).map((v) => d.values[v]!)
  })
  if (g.dims.length === 1 && selection[g.dims[0]!.name]!.length === 1) return `${g.dims[0]!.name}=${selection[g.dims[0]!.name]![0]}`
  return JSON.stringify(selection)
}

/** The filter that shows an issue's keys in the table. */
export function issueFilter(issue: IssueGroup): Pick<KeyFilter, 'states' | 'error'> {
  return issue.kind === 'error' ? { states: ['failed'], error: issue.error } : { states: [issue.kind], error: null }
}

/** Exact rectangular selection only: never silently run a cross-product outside the view. */
export function exactSelection(g: Grid, cells: readonly number[]): string | null {
  if (cells.length === 0) return null
  const values = g.dims.map((d, i) => [...new Set(cells.map(cell => Math.floor(cell / g.strides[i]!) % d.values.length))].sort((a, b) => a - b))
  if (values.reduce((n, v) => n * v.length, 1) !== cells.length) return null
  return JSON.stringify(Object.fromEntries(g.dims.map((d, i) => [d.name, values[i]!.map(v => d.values[v]!)])))
}
