import { useEffect } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { runIsLive } from '@/lib/runs'

export function useRuns(limit: number) {
  return useQuery({
    queryKey: ['runs', 'list', limit],
    queryFn: () => api.runs(limit),
    refetchInterval: 2_000,
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
    refetchInterval: (q) => !q.state.data || runIsLive(q.state.data.run.status) ? 1_000 : false,
    retry: false,
  })
  const status = query.data?.run.status
  useEffect(() => {
    if (status && !runIsLive(status)) {
      void client.invalidateQueries({ queryKey: ['state'] })
      void client.invalidateQueries({ queryKey: ['asset'] })
      void client.invalidateQueries({ queryKey: ['runs', 'list'] })
    }
  }, [status, client])
  return query
}
