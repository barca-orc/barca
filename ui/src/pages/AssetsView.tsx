import { Navigate, useSearchParams } from 'react-router'
import { AssetsPage } from './AssetsPage'
import { GraphPage } from './GraphPage'
import { GroupedPipelinePage } from './GroupedPipelinePage'
import { useGroups } from '@/hooks/useGroups'

export function AssetsView() {
  const [params] = useSearchParams()
  const groups = useGroups()
  const pipeline = params.get('pipeline')
  if (groups.data?.some(g => !pipeline || g.id.startsWith(`group:${pipeline}:`))) return <GroupedPipelinePage />
  return params.get('view') === 'graph' ? <GraphPage /> : <AssetsPage />
}

/** Keep old graph bookmarks working while Assets owns both views. */
export function LegacyGraph() {
  const [params] = useSearchParams()
  const next = new URLSearchParams(params)
  next.set('view', 'graph')
  return <Navigate to={`/assets?${next}`} replace />
}
