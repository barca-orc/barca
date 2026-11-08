import type { ReactNode } from 'react'
import { StatusGlyph } from '@/components'
import { CACHE_STATES, LAST_ATTEMPT_FAILED, PINNED, cacheStateGlyph, cacheStateLabel } from '@/lib/cacheStateDisplay'
import type { Glyph } from '@/lib/glyph'

/** Every state a node can show, in the CLI's words and the order the legend lists them. */
const ENTRIES: readonly (readonly [Glyph, string])[] = [
  ['running', 'running'],
  ['failed', LAST_ATTEMPT_FAILED],
  ['pinned', PINNED],
  ...CACHE_STATES.map((state) => [cacheStateGlyph(state), cacheStateLabel(state)] as const),
]

/**
 * What the graph's glyphs mean, in the CLI's words, for the states it shows now: `shown` maps
 * each glyph on screen to a node's word for it. A run's step states (queued, skipped) keep the
 * word of their node; the waiting glyph is not listed.
 */
export function GraphLegend({ shown, children }: { shown: ReadonlyMap<string, string>; children?: ReactNode }) {
  const known = ENTRIES.filter(([glyph]) => shown.has(glyph))
  const listed = new Set(known.map(([glyph]) => glyph))
  const others = [...shown].filter(([glyph]) => !listed.has(glyph as Glyph) && glyph !== 'pending') as [Glyph, string][]
  const entries = [...known, ...others]
  if (entries.length === 0 && !children) return null
  return (
    <div className="barca-legend" aria-label="Legend">
      {children}
      {entries.map(([glyph, word]) => (
        <span key={glyph}><StatusGlyph glyph={glyph} size={12} />{word}</span>
      ))}
    </div>
  )
}
