/**
 * The geometry of the key explorer's overview: the served rows of a partitioned node's keys
 * (`GET /assets/{id}/partitions/overview`, what `barca status NODE --overview` prints) cut into
 * as many columns as fit, and the key a column of one value stands for. Layout only: which
 * keys exist, each key's state, what is wrong and where, and which keys a filter keeps are
 * served (`barca-core`, `key_query`).
 */

import { KEY_STATES, keyStateOfCode, type KeyState } from './keyStateDisplay'
import type { KeysOverview, OverviewRow } from './types'

/** The values of the dimension the overview's rows run along, in order; empty with no keys. */
export function alongValues(overview: KeysOverview): readonly string[] {
  return overview.dimensions.find((d) => d.name === overview.along)?.values ?? []
}

/** A range's index bounds along the rows' dimension, or null when it names no value of it. */
export function rangeBounds(values: readonly string[], range: { from: string; to: string } | null): [number, number] | null {
  if (!range) return null
  const a = values.indexOf(range.from)
  const b = values.indexOf(range.to)
  if (a < 0 || b < 0) return null
  return a <= b ? [a, b] : [b, a]
}

/** The key one cell of a row stands for, as the server spells it: `day=2026-01-01,region=apac`. */
export function cellKey(overview: KeysOverview, row: OverviewRow, index: number): string {
  const along = alongValues(overview)
  let other = 0
  return overview.dimensions
    .map((d) => `${d.name}=${d.name === overview.along ? along[index] : row.values[other++]}`)
    .join(',')
}

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
  /** One row per served row. */
  rows: Array<{ label: string; bins: HeatBin[] }>
  /** Labels under the columns: the long dimension's value where each starts, a few of them. */
  ticks: Array<{ bin: number; label: string }>
  /** The stretch drawn, inclusive. */
  window: [number, number]
}

function noCounts(): Record<KeyState, number> {
  return Object.fromEntries(KEY_STATES.map((s) => [s, 0])) as Record<KeyState, number>
}

/**
 * Every key in at most `columns` columns: each served row's long dimension cut into equal
 * stretches, each drawn in the most urgent state of its keys. `window` zooms to a stretch of the
 * long dimension.
 */
export function heatmap(overview: KeysOverview, columns: number, window: [number, number] | null = null, maxTicks = 6): Heatmap {
  const values = alongValues(overview)
  const [lo, hi] = window ?? [0, Math.max(0, values.length - 1)]
  const n = hi - lo + 1
  const cols = Math.max(1, Math.min(columns, n))
  const edges = Array.from({ length: cols + 1 }, (_, b) => lo + Math.floor((b * n) / cols))
  const rows = overview.rows.map((row) => ({
    label: row.values.join(', '),
    bins: Array.from({ length: cols }, (_, b): HeatBin => {
      const counts = noCounts()
      for (let i = edges[b]!; i < edges[b + 1]!; i++) {
        const s = keyStateOfCode(row.states[i])
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
  return { rows, ticks, window: [lo, hi] }
}
