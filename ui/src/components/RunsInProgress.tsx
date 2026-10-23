import { Link } from 'react-router'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useHealth } from '@/hooks/useHealth'
import { healthQueueLabel } from '@/lib/queue'
import { runsInProgress, runsInProgressLabel } from '@/lib/running'
import { StatusDot } from './StatusDot'

/**
 * barca · RunsInProgress
 * The header chip: how many runs are working on this project now, whoever
 * started them, and how many wait in the server's queue (and whether it is
 * paused), linking to Runs. Hidden while nothing runs or waits.
 */
export function RunsInProgress() {
  const { data } = useAssetStates()
  const health = useHealth()
  const count = runsInProgress(data ?? []).length
  const queue = healthQueueLabel(health.data?.queue)
  if (count === 0 && queue === null) return null
  return (
    <Link to="/runs" className="barca-runs-chip" role="status">
      <StatusDot status={count > 0 ? 'running' : 'queued'} size={6} />
      {count > 0 && runsInProgressLabel(count)}
      {count > 0 && queue && ' · '}
      {queue}
    </Link>
  )
}
