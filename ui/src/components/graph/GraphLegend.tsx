import type { ReactNode } from 'react'
import { StatusDot } from '@/components'
import { CACHE_STATES, FAILED_TONE, LAST_ATTEMPT_FAILED, cacheStateTone } from '@/lib/cacheStateDisplay'

/**
 * What the graph's colours mean, in the CLI's words: run work, each cache state (the eight words
 * of `barca status`), and the fact beside a state, a failed last attempt. The legend is the
 * display table itself, so it cannot drift from the nodes.
 */
export function GraphLegend({ children }: { children?: ReactNode }) {
  return (
    <div className="barca-legend">
      {children}
      <span><StatusDot tone="running" live size={6} />running</span>
      {CACHE_STATES.map((state) => (
        <span key={state}><StatusDot tone={cacheStateTone(state)} size={6} />{state}</span>
      ))}
      <span><StatusDot tone={FAILED_TONE} size={6} />{LAST_ATTEMPT_FAILED}</span>
    </div>
  )
}
