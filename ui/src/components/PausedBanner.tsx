import { Link } from 'react-router'
import { PauseCircle } from 'lucide-react'
import { usePauses } from '@/hooks/usePauses'
import { pausedBanner } from '@/lib/pauses'

/**
 * barca · PausedBanner
 * Across the top of every page while any asset is paused (`barca pause`): scheduled runs skip
 * them, so it is never out of sight. Links to the paused assets.
 */
export function PausedBanner() {
  const { data } = usePauses()
  const text = pausedBanner(data?.total ?? 0)
  if (!text) return null
  return (
    <div role="status" className="barca-offline barca-paused-banner">
      <PauseCircle size={14} aria-hidden />
      <span><strong>{text.headline}</strong> {text.detail}</span>
      <Link className="barca-offline-retry" to="/assets?paused=1">Show paused</Link>
    </div>
  )
}
