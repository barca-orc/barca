import type { ReactNode } from 'react'
import { StatusGlyph } from '@/components'
import { CauseDot } from '@/components/CauseDot'
import { MAP_PARTS, mapPartLabel, mapPartMeaning, type MapPart } from '@/lib/causeDisplay'
import type { Glyph } from '@/lib/glyph'

/** What the graph shows now: the causes (and fresh) its cards rest in, and the glyphs of its run work and pins. */
export interface LegendShown {
  parts: ReadonlySet<MapPart>
  /** A glyph on screen that is not a cause's, with its word: `running`, `pinned`. */
  glyphs: ReadonlyMap<Glyph, string>
}

/**
 * What the graph's marks mean, in the health map's words and colours, for the states it shows
 * now: each cause on screen (and fresh) in the served order, then the glyphs of run work and
 * pins. A run's waiting glyph is not listed.
 */
export function GraphLegend({ shown, children }: { shown: LegendShown; children?: ReactNode }) {
  const parts = MAP_PARTS.filter((part) => shown.parts.has(part))
  const glyphs = [...shown.glyphs].filter(([glyph]) => glyph !== 'pending')
  if (parts.length === 0 && glyphs.length === 0 && !children) return null
  return (
    <div className="barca-legend" aria-label="Legend">
      {children}
      {glyphs.map(([glyph, word]) => (
        <span key={glyph}><StatusGlyph glyph={glyph} size={12} />{word}</span>
      ))}
      {parts.map((part) => (
        <span key={part} title={mapPartMeaning(part)}><CauseDot part={part} />{mapPartLabel(part)}</span>
      ))}
    </div>
  )
}
