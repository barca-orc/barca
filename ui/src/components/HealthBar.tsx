import { Fragment } from 'react'
import { HEALTH_PARTS, healthLabel, healthParts, healthPartGlyph, healthPartTone, partCount, type ScopeHealth } from '@/lib/health'
import { glyphColor } from '@/lib/glyph'

export interface HealthBarProps {
  /** The scope's entry of the rollup (`GET /state/rollup`): its nodes by health, and run work. */
  scope: ScopeHealth
  /** How many parts the label names, most urgent first (the tooltip names them all). */
  labelLimit?: number
  /** Bar width in px; the label follows it. */
  width?: number
}

/**
 * barca · HealthBar
 * A scope's nodes by health, as one proportional bar (a segment per health present, in the hue
 * of its glyph) and a label in the CLI's words, most urgent first: `2 failing · 121 behind ·
 * 380 ok`. It counts nodes (assets, tasks and sensors), and its tooltip says so: the health map
 * beside it is sized in assets, from the same entry. Run work is a part of its own beside the
 * healths. A segment that needs attention always gets a visible sliver, however many ok nodes
 * surround it.
 */
export function HealthBar({ scope, labelLimit = 2, width = 96 }: HealthBarProps) {
  const total = Math.max(1, scope.nodes)
  const present = HEALTH_PARTS.filter((p) => partCount(scope, p) > 0)
  const parts = healthParts(scope, labelLimit)
  const label = healthLabel(scope)
  const title = label ? `${scope.nodes.toLocaleString()} ${scope.nodes === 1 ? 'node' : 'nodes'}: ${label}` : ''
  return (
    <span className="barca-health" title={title}>
      <span className="barca-health-bar" style={{ width }} role="img" aria-label={title || 'empty'}>
        {present.map((p) => {
          const tone = healthPartTone(p)
          return <span key={p} data-part={p} data-tone={tone} style={{ background: glyphColor(healthPartGlyph(p)), flexGrow: partCount(scope, p) / total, minWidth: tone === 'neutral' || tone === 'success' ? 1 : 3 }} />
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
