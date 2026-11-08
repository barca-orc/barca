import { useAssetStates } from '@/hooks/useAssetStates'
import { useHealth } from '@/hooks/useHealth'
import { healthQueueLabel } from '@/lib/queue'
import { runsInProgress, runsInProgressLabel } from '@/lib/running'
import { StatusGlyph } from './StatusGlyph'

/**
 * barca · RunsInProgress
 * Beside Runs in the sidebar: how many runs are working on this project now, whoever started
 * them, and how many wait in the server's queue (and whether it is paused), in its tooltip.
 * Hidden while nothing runs or waits.
 */
export function RunsInProgress() {
  const { data } = useAssetStates()
  const health = useHealth()
  const count = runsInProgress(data ?? []).length
  const queue = healthQueueLabel(health.data?.queue)
  if (count === 0 && queue === null) return null
  const label = [count > 0 ? runsInProgressLabel(count) : null, queue].filter(Boolean).join(' · ')
  return (
    <span className="barca-nav-live" role="status" title={label} aria-label={label}>
      <StatusGlyph glyph={count > 0 ? 'running' : 'queued'} live={count > 0} size={12} />
      {count > 0 ? count : null}
    </span>
  )
}
