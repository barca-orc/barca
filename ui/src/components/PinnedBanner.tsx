import { Link } from 'react-router'
import { Pin } from 'lucide-react'
import { usePins } from '@/hooks/usePins'
import { shortName } from '@/lib/graph'
import { plural } from '@/lib/plural'
import { pinnedBanner } from '@/lib/pins'

/**
 * barca · PinnedBanner
 * Across the top of every page while any asset is pinned (`barca pin`): runs read the pinned
 * versions instead of computing them, so it is never out of sight. Links to the pinned assets.
 */
export function PinnedBanner() {
  const { data } = usePins()
  const keys = new Map<string, number>()
  let assets = 0
  for (const pin of data?.pins ?? []) {
    const at = pin.node_id.indexOf('[')
    if (at < 0) assets++
    else {
      const node = pin.node_id.slice(0, at)
      keys.set(node, (keys.get(node) ?? 0) + 1)
    }
  }
  const text = pinnedBanner(assets)
  if (!text && keys.size === 0) return null
  return (
    <div role="status" className="barca-offline barca-paused-banner barca-pinned-banner">
      <Pin size={14} aria-hidden />
      {text && <>
        <span><strong>{text.headline}</strong> {text.detail}</span>
        <Link className="barca-offline-retry" to="/assets?pinned=1">Show pinned assets</Link>
      </>}
      {[...keys].map(([node, count]) => <Link key={node} className="barca-offline-retry"
        to={`/keys?node=${encodeURIComponent(node)}&state=pinned`}>
        {plural(count, 'key')} pinned in {shortName(node)}
      </Link>)}
    </div>
  )
}
