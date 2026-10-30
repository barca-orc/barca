import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api, type StateScope } from '@/lib/api'
import { statePollInterval } from '@/lib/running'
import { useSearchParams } from 'react-router'
import { useAssets } from './useAssets'
import { useGroups } from './useGroups'
import { inPipeline } from '@/lib/pipeline'
import { groupTree, viewScope } from '@/lib/groups'

/** The query key of `scope`'s state; `null` is the whole project. */
export function stateKey(scope: StateScope | null) {
  return ['state', scope] as const
}

/** What the URL has open: a group, else a pipeline (source file), else everything. */
export function useStateScope(): { scope: StateScope | null; waiting: boolean } {
  const [params] = useSearchParams()
  const pipeline = params.get('pipeline')
  const groups = useGroups()
  const tree = useMemo(
    () => groupTree((groups.data ?? []).filter(g => !pipeline || g.id.startsWith(`group:${pipeline}:`))),
    [groups.data, pipeline],
  )
  const group = viewScope(params, tree)
  return {
    scope: group ? { group } : pipeline ? { file: pipeline } : null,
    // A group in the URL is only known to be one once the groups have loaded.
    waiting: params.get('flat') !== '1' && params.has('group') && groups.isPending,
  }
}

/**
 * The open pipeline's or group's state from `GET /state?files=` / `?targets=<group id>`, or
 * every node's on All pipelines: the server expands a group or file to its nodes, so the
 * request stays one short URL at any size. Refreshed every 10s, and every 2s while some run
 * is working on a node (whoever started it), so every tab sees every run. Unchanged polls are
 * `304`s (ETag). No placeholder data: a status table showing invented states would be worse
 * than showing nothing.
 */
export function useAssetStates() {
  const { scope, waiting } = useStateScope()
  const assets = useAssets()
  // A pipeline with no nodes is an empty result, not a request the server would refuse.
  const listed = assets.isPlaceholderData ? undefined : assets.data
  const file = scope && 'file' in scope ? scope.file : null
  const empty = file !== null && listed !== undefined && !listed.some((a) => inPipeline(a.id, file))
  return useQuery({
    queryKey: stateKey(scope),
    queryFn: ({ signal }) => (empty ? Promise.resolve([]) : api.state(scope, signal)),
    enabled: !waiting && (file === null || listed !== undefined),
    refetchInterval: (query) => statePollInterval(query.state.data),
    retry: false,
  })
}

/**
 * The last whole-project state this tab loaded, without asking for it: what the sidebar
 * shows for the pipelines outside the open scope.
 */
export function useLastWholeState() {
  return useQuery({ queryKey: stateKey(null), queryFn: () => api.state(null), enabled: false })
}
