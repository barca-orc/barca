import type { CSSProperties } from 'react'
import type { Glyph } from '@/lib/glyph'
import { StatusGlyph } from './StatusGlyph'

export interface StatusBadgeProps {
  /** How the state is drawn (`lib/glyph.ts`). */
  glyph: Glyph
  /** The words: a node's state (`failing`, `never run`) or a run's status. */
  label: string
  size?: 'sm' | 'md'
  /** Live work: the running glyph turns. */
  live?: boolean
  style?: CSSProperties
}

/**
 * barca · StatusBadge
 * A state's glyph and its word, and nothing else: no pill, no tinted ground, no coloured text.
 * The glyph carries the colour; the word stays in the text colour around it, so a column of
 * states reads calmly and a failure stands out by its shape and hue alone.
 */
export function StatusBadge({ glyph, label, size = 'md', live = false, style }: StatusBadgeProps) {
  return (
    <span className="barca-badge" data-glyph={glyph} data-size={size} style={style}>
      <StatusGlyph glyph={glyph} live={live} size={size === 'sm' ? 12 : 14} />
      {label}
    </span>
  )
}
