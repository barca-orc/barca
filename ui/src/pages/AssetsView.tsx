import { Navigate, useSearchParams } from 'react-router'
import { AssetsPage } from './AssetsPage'
import { GraphPage } from './GraphPage'
import { GroupedPipelinePage } from './GroupedPipelinePage'
import { useAssets } from '@/hooks/useAssets'

export function AssetsView() {
  const [params] = useSearchParams()
  const assets = useAssets()
  const pipeline = params.get('pipeline')
  if (pipeline === 'modeling.py' || (!pipeline && !assets.isPlaceholderData && assets.data?.some(a => a.id.startsWith('modeling.py:')))) return <GroupedPipelinePage />
  return params.get('view') === 'graph' ? <GraphPage /> : <AssetsPage />
}

/** Keep old graph bookmarks working while Assets owns both views. */
export function LegacyGraph() {
  const [params] = useSearchParams()
  const next = new URLSearchParams(params)
  next.set('view', 'graph')
  return <Navigate to={`/assets?${next}`} replace />
}
