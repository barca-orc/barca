import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { statePollInterval } from '@/lib/running'

/**
 * Every node's state from `GET /state`, refreshed every 10s, and every 2s
 * while some run is working on a node (whoever started it), so every tab sees
 * every run. No placeholder data: unlike the graph, a status table showing
 * invented states would be worse than showing nothing.
 */
export function useAssetStates() {
  return useQuery({
    queryKey: ['state'],
    queryFn: api.state,
    refetchInterval: (query) => statePollInterval(query.state.data),
    retry: false,
  })
}
