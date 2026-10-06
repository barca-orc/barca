import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

/** Poll the server liveness endpoint. Drives the connection indicator. */
export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    refetchInterval: 10_000,
    retry: false,
  })
}
