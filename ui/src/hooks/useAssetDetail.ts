import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

/** One node's summary + run history (`GET /assets/{id}`), while a panel shows it. */
export function useAssetDetail(id: string | null) {
  return useQuery({
    queryKey: ['asset', id],
    queryFn: () => api.asset(id!),
    enabled: id !== null,
    refetchInterval: 10_000,
    retry: false,
  })
}
