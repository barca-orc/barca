import { Fragment } from 'react'
import { MAP_PARTS, causeParts, mapPartColor } from '@/lib/causeDisplay'
import { plural } from '@/lib/plural'
import type { RollupEntry } from '@/lib/rollupView'

/** What a bar reads of a scope's rollup entry: nothing is counted or weighed here. */
export type BarScope = Pick<RollupEntry, 'nodes' | 'not_fresh' | 'causes' | 'weights' | 'running'>

export interface HealthBarProps {
  /** The scope's entry of the rollup (`GET /state/rollup`). */
  scope: BarScope
  /** How many causes the label names, in the served order (the tooltip names them all). */
  labelLimit?: number
  /** Bar width in px; the label follows it. */
  width?: number
  /** Leave the run work out of the label: for a place that already says it beside the bar. */
  quiet?: boolean
}

/**
 * barca · HealthBar
 * A scope's distance from fresh as one bar, in the words and colours of the health map: a
 * segment per cause inside it and one for what is fresh, each as wide as its part of the
 * entry's served `weights`, then the causes in words in the served order (`1 failing · 84
 * catching up`), `all 116 fresh` when nothing is behind, and how many nodes a run is working
 * on. It counts nodes, and its tooltip says so. A cause always gets a visible sliver, however
 * many fresh nodes surround it.
 */
export function HealthBar({ scope, labelLimit = 2, width = 96, quiet = false }: HealthBarProps) {
  const present = MAP_PARTS.filter((p) => scope.weights[p] > 0)
  const causes = causeParts(scope.causes)
  const all = causes.map((p) => `${p.count.toLocaleString()} ${p.label}`).join(' · ')
  const fresh = scope.not_fresh === 0 && scope.nodes > 0 ? `all ${scope.nodes.toLocaleString()} fresh` : null
  const running = scope.running > 0 ? `${scope.running.toLocaleString()} running` : null
  const title = scope.nodes === 0 ? '' : `${plural(scope.nodes, 'node')}: ${[fresh ?? `${scope.not_fresh.toLocaleString()} not fresh (${all})`, running].filter(Boolean).join(' · ')}`
  return (
    <span className="barca-health" title={title}>
      <span className="barca-health-bar" style={{ width }} role="img" aria-label={title || 'empty'}>
        {present.map((p) => (
          <span key={p} data-part={p} style={{ background: mapPartColor(p), flexGrow: scope.weights[p], minWidth: p === 'fresh' ? 1 : 3 }} />
        ))}
      </span>
      <span className="barca-health-label">
        {scope.nodes === 0 ? 'empty' : (
          <>
            {fresh && <span data-key="fresh">{fresh}</span>}
            {causes.slice(0, labelLimit).map((p, i) => (
              <Fragment key={p.cause}>{i > 0 && ' · '}<span data-key={p.cause} data-alarm={p.alarm || undefined}>{p.count.toLocaleString()} {p.label}</span></Fragment>
            ))}
            {causes.length > labelLimit && <span className="barca-cell-sub"> · {causes.length - labelLimit} more</span>}
            {running && !quiet && <> · <span data-key="running">{running}</span></>}
          </>
        )}
      </span>
    </span>
  )
}
