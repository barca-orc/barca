import { useEffect } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import type { RunsQuery } from '@/lib/types'
import { runIsLive, runsPollInterval } from '@/lib/runs'
import { recovering } from './polling'
import type { RunList, RunView } from '@/lib/types'

/** `GET /runs` for `query`: the filters are the server's, so each one is its own request. */
export function useRuns(query: RunsQuery) {
  return useQuery({
    queryKey: ['runs', 'list', query],
    queryFn: () => api.runs(query),
    // Changing a filter keeps the list on screen until the filtered one arrives.
    placeholderData: (previous) => previous,
    // Fast while a listed run is live; a run started elsewhere shows within the idle interval.
    refetchInterval: recovering<RunList>((q) => runsPollInterval(q.state.data?.runs)),
    retry: false,
  })
}

export function useRunDetail(id: string | null) {
  const client = useQueryClient()
  const query = useQuery({
    queryKey: ['runs', 'detail', id],
    queryFn: () => api.runDetail(id!),
    enabled: id !== null,
    // Continue polling while queued/running, also when a transient fetch fails.
    refetchInterval: recovering<RunView>((q) => !q.state.data || runIsLive(q.state.data.status) ? 1_000 : false),
    retry: false,
  })
  const status = query.data?.status
  useEffect(() => {
    if (status && !runIsLive(status)) {
      void client.invalidateQueries({ queryKey: ['state'] })
      void client.invalidateQueries({ queryKey: ['rollup'] })
      void client.invalidateQueries({ queryKey: ['asset'] })
      void client.invalidateQueries({ queryKey: ['runs', 'list'] })
    }
  }, [status, client])
  return query
}
