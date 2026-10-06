import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Every node's state from `GET /state`, refreshed every 10s. No placeholder
 * data: unlike the graph, a status table showing invented states would be
 * worse than showing nothing.
 */
export function useAssetStates() {
  return useQuery({
    queryKey: ['state'],
    queryFn: api.state,
    refetchInterval: 10_000,
    retry: false,
  })
}
