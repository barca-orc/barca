import type { ReactNode } from 'react'
import { StatusGlyph } from '@/components'
import { CACHE_STATES, LAST_ATTEMPT_FAILED, PINNED, cacheStateGlyph, cacheStateLabel } from '@/lib/cacheStateDisplay'

/**
 * What the graph's glyphs mean, in the CLI's words: run work, a failed last attempt, a pinned
 * asset, and each cache state (the eight words of `barca status`). The legend is the display
 * table itself, so it cannot drift from the nodes; each state has its own shape and hue.
 */
export function GraphLegend({ children }: { children?: ReactNode }) {
  return (
    <div className="barca-legend">
      {children}
      <span><StatusGlyph glyph="running" size={12} />running</span>
      <span><StatusGlyph glyph="failed" size={12} />{LAST_ATTEMPT_FAILED}</span>
      <span><StatusGlyph glyph="pinned" size={12} />{PINNED}</span>
      {CACHE_STATES.map((state) => (
        <span key={state}><StatusGlyph glyph={cacheStateGlyph(state)} size={12} />{cacheStateLabel(state)}</span>
      ))}
    </div>
  )
}
