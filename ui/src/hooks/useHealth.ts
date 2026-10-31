import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/** Poll the server liveness endpoint. Drives the connection indicator. */
export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    refetchInterval: recovering(10_000),
    retry: false,
  })
}
