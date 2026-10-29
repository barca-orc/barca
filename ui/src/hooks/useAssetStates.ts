import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { statePollInterval } from '@/lib/running'
import { useSearchParams } from 'react-router'
import { useAssets } from './useAssets'
import { useGroups } from './useGroups'
import { groupLeaves, groupTree } from '@/lib/groups'
import { inPipeline } from '@/lib/pipeline'

/**
 * The open pipeline/group's state from `GET /state?targets=`, or every node on All pipelines.
 * Refreshed every 10s, and every 2s
 * while some run is working on a node (whoever started it), so every tab sees
 * every run. No placeholder data: unlike the graph, a status table showing
 * invented states would be worse than showing nothing.
 */
export function useAssetStates() {
  const [params] = useSearchParams()
  const pipeline = params.get('pipeline')
  const assets = useAssets()
  const groups = useGroups()
  const tree = groupTree((groups.data ?? []).filter(g => !pipeline || g.id.startsWith(`group:${pipeline}:`)))
  const selected = params.get(params.get('view') === 'graph' ? 'focus' : 'node')
  const requestedGroup = params.get('group') ?? tree.parents.get(selected ?? '')
  const group = params.get('flat') !== '1' && tree.groups.has(requestedGroup ?? '')
    ? requestedGroup : null
  const waitingForGroup = params.get('flat') !== '1' && params.has('group') && groups.isPending
  const targets = group
    ? groupLeaves(group, tree).sort()
    : pipeline
      ? (assets.isPlaceholderData ? [] : assets.data ?? []).filter(a => inPipeline(a.id, pipeline)).map(a => a.id).sort()
      : undefined
  return useQuery({
    queryKey: ['state', targets ?? null],
    queryFn: ({ signal }) => {
      if (pipeline && !group && assets.isError) throw assets.error
      // No nodes in this file is an empty result, not an unscoped request or an endless load.
      return targets?.length === 0 ? Promise.resolve([]) : api.state(targets, signal)
    },
    enabled: !waitingForGroup && !(pipeline && !group && assets.isPlaceholderData),
    refetchInterval: (query) => statePollInterval(query.state.data),
    retry: false,
  })
}
