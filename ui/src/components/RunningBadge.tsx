import { Link } from 'react-router'
import { runningBadge } from '@/lib/running'
import type { NodeRunning } from '@/lib/types'
import { StatusBadge } from './StatusBadge'

export interface RunningBadgeProps {
  running: NodeRunning
  /** Now, for the elapsed time (ms since epoch). */
  nowMs: number
  size?: 'sm' | 'md'
}

/**
 * barca · RunningBadge
 * A node a run is working on now: a pulsing Running (or Queued) badge with the
 * elapsed time and keys done, linking to the run.
 */
export function RunningBadge({ running, nowMs, size = 'sm' }: RunningBadgeProps) {
  const badge = runningBadge(running, nowMs)
  return (
    <Link
      to={badge.href}
      className="barca-running-badge"
      title={badge.hint}
      aria-label={`${badge.label}: open run ${running.run_id}`}
      onClick={(event) => event.stopPropagation()}
    >
      <StatusBadge status={badge.status} label={badge.label} size={size} />
    </Link>
  )
}
