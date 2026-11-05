import { Navigate, useLocation, useSearchParams } from 'react-router'
import { AssetsPage } from './AssetsPage'
import { GraphPage } from './GraphPage'
import { GroupedPipelinePage } from './GroupedPipelinePage'
import { useGroups } from '@/hooks/useGroups'
import { ChainPage } from './ChainPage'
import { ChainsOverview } from './ChainsOverview'
import { useChains } from '@/hooks/useChains'
import { useAssets } from '@/hooks/useAssets'

export function AssetsView() {
  const [params] = useSearchParams()
  const groups = useGroups()
  const pipeline = params.get('pipeline')
  const chain = params.get('chain')
  if (params.get('chains') === 'all') return <ChainsOverview />
  if (chain) return <ChainPage chainIds={chain.split(',').filter(Boolean)} />
  if (groups.data?.some(g => !pipeline || g.id.startsWith(`group:${pipeline}:`))) return <GroupedPipelinePage />
  return params.get('view') === 'graph' ? <GraphPage /> : <AssetsPage />
}

/** `#/` opens Assets, and a deep link to it keeps its filters. */
export function IndexRedirect() {
  const { search } = useLocation()
  const assets = useAssets()
  const chains = useChains()
  // A project with chains opens on them; a deep link keeps its filters.
  if (!search && (assets.isPending || assets.isPlaceholderData)) return null
  if (!search && chains.length > 0) return <Navigate to="/assets?chains=all" replace />
  return <Navigate to={{ pathname: '/assets', search }} replace />
}

/** Keep old graph bookmarks working while Assets owns both views. */
export function LegacyGraph() {
  const [params] = useSearchParams()
  const next = new URLSearchParams(params)
  next.set('view', 'graph')
  return <Navigate to={`/assets?${next}`} replace />
}
