import { Link } from 'react-router'
import { useAssetStates } from '@/hooks/useAssetStates'
import { runsInProgress, runsInProgressLabel } from '@/lib/running'
import { StatusDot } from './StatusDot'

/**
 * barca · RunsInProgress
 * The header chip: how many runs are working on this project now, whoever
 * started them, linking to Runs. Hidden while nothing runs.
 */
export function RunsInProgress() {
  const { data } = useAssetStates()
  const count = runsInProgress(data ?? []).length
  if (count === 0) return null
  return (
    <Link to="/runs" className="barca-runs-chip" role="status">
      <StatusDot status="running" size={6} />
      {runsInProgressLabel(count)}
    </Link>
  )
}
