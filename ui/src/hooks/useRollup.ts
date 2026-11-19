import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api, type StateScope } from '@/lib/api'
import { POLL_IDLE_MS, POLL_RUNNING_MS } from '@/lib/running'
import { EMPTY_ROLLUP, ROOT, indexRollup, type Rollup, type RollupTree } from '@/lib/rollupView'
import { recovering } from './polling'

export interface ProjectRollup {
  tree: RollupTree
  /** The rollup has loaded once. */
  loaded: boolean
}

/**
 * The rollup of a pipeline (`GET /state/rollup?files=`), or of the whole project: one tree
 * (root, groups, files, nodes) summed in `barca-service`, the same response `barca status
 * --rollup` prints. Every count a page shows for a scope is an entry of it, and the health map
 * draws it, so the map, the bars and the CLI agree. Refreshed like the state it sums: every
 * 10s, every 2s while a run is working on a node in it, and when a run starts or finishes
 * (runs invalidate it with `state`).
 */
export function useRollup(pipeline: string | null): ProjectRollup {
  const scope: StateScope | null = pipeline ? { file: pipeline } : null
  const query = useQuery({
    queryKey: ['rollup', scope] as const,
    queryFn: ({ signal }) => api.rollup(scope, signal),
    refetchInterval: recovering<Rollup>((q) => ((q.state.data?.entries[0]?.running ?? 0) > 0 ? POLL_RUNNING_MS : POLL_IDLE_MS)),
    retry: false,
  })
  const tree = useMemo(() => (query.data ? indexRollup(query.data) : EMPTY_ROLLUP), [query.data])
  return { tree, loaded: !!query.data && tree.entries.has(ROOT) }
}
