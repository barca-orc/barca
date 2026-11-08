import { Link } from 'react-router'
import { Pin } from 'lucide-react'
import { usePins } from '@/hooks/usePins'
import { pinnedBanner } from '@/lib/pins'

/**
 * barca · PinnedBanner
 * Across the top of every page while any asset is pinned (`barca pin`): runs read the pinned
 * versions instead of computing them, so it is never out of sight. Links to the pinned assets.
 */
export function PinnedBanner() {
  const { data } = usePins()
  const text = pinnedBanner(data?.pins.length ?? 0)
  if (!text) return null
  return (
    <div role="status" className="barca-offline barca-paused-banner barca-pinned-banner">
      <Pin size={14} aria-hidden />
      <span><strong>{text.headline}</strong> {text.detail}</span>
      <Link className="barca-offline-retry" to="/assets?pinned=1">Show pinned</Link>
    </div>
  )
}
