import { queryOptions, useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { mockAssets } from '@/mocks/assets'
import { recovering } from './polling'

/** The asset graph's query (`GET /assets`), shared by the hook and its tests. */
export function assetsQuery() {
  return queryOptions({
    queryKey: ['assets'],
    queryFn: api.assets,
    placeholderData: mockAssets,
    staleTime: 5_000,
    // The graph rarely changes; a failed fetch is asked again until the server answers.
    refetchInterval: recovering(),
    retry: false,
  })
}

/**
 * Fetch the asset graph from barca-server. Falls back to mock data as
 * placeholder so the UI renders immediately, even before `barca serve`
 * is running.
 */
export function useAssets() {
  return useQuery(assetsQuery())
}
