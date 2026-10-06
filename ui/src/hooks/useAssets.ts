import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { mockAssets } from '@/mocks/assets'

/**
 * Fetch the asset graph from barca-server. Falls back to mock data as
 * placeholder so the UI renders immediately, even before `barca serve`
 * is running.
 */
export function useAssets() {
  return useQuery({
    queryKey: ['assets'],
    queryFn: api.assets,
    placeholderData: mockAssets,
    staleTime: 5_000,
    retry: false,
  })
}
