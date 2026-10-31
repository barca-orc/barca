import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/** One node's summary + run history (`GET /assets/{id}`), while a panel shows it. */
export function useAssetDetail(id: string | null) {
  return useQuery({
    queryKey: ['asset', id],
    queryFn: () => api.asset(id!),
    enabled: id !== null,
    refetchInterval: recovering(10_000),
    retry: false,
  })
}
