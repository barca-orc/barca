import { Fragment } from 'react'
import { HEALTH_PARTS, healthLabel, healthParts, healthPartGlyph, healthPartTone, type HealthCounts } from '@/lib/health'
import { glyphColor } from '@/lib/glyph'

export interface HealthBarProps {
  counts: HealthCounts
  /** How many parts the label names, most urgent first (the tooltip names them all). */
  labelLimit?: number
  /** Bar width in px; the label follows it. */
  width?: number
}

/**
 * barca · HealthBar
 * A scope's nodes by cache state, as one proportional bar (a segment per state present, in the
 * hue of the state's glyph) and a label in the CLI's words, most urgent first: `2 last attempt failed · 121
 * stale · 380 cached`. A running segment, or one of a state that needs attention, always gets a
 * visible sliver, however many cached nodes surround it.
 */
export function HealthBar({ counts, labelLimit = 2, width = 96 }: HealthBarProps) {
  const total = Math.max(1, counts.total)
  const present = HEALTH_PARTS.filter((p) => (p === 'running' || p === 'pending' ? counts[p] : counts.states[p]) > 0)
  const parts = healthParts(counts, labelLimit)
  const title = healthLabel(counts)
  return (
    <span className="barca-health" title={title}>
      <span className="barca-health-bar" style={{ width }} role="img" aria-label={title || 'empty'}>
        {present.map((p) => {
          const count = p === 'running' || p === 'pending' ? counts[p] : counts.states[p]
          const tone = healthPartTone(p)
          return <span key={p} data-part={p} data-tone={tone} style={{ background: glyphColor(healthPartGlyph(p)), flexGrow: count / total, minWidth: tone === 'neutral' || tone === 'success' ? 1 : 3 }} />
        })}
      </span>
      <span className="barca-health-label">
        {parts.length === 0 ? 'empty' : parts.map((p, i) => (
          <Fragment key={p.key}>{i > 0 && ' · '}<span data-key={p.key}>{p.text}</span></Fragment>
        ))}
      </span>
    </span>
  )
}
