import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import type { ServerHealth } from '@/lib/types'
import { recovering } from './polling'

/** Poll the server liveness endpoint. Drives the connection indicator. */
export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    refetchInterval: recovering<ServerHealth>((q) => (q.state.data?.queue?.running ?? 0) > 0 ? 2_000 : 10_000),
    retry: false,
  })
}
