import { queryOptions, useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/** The asset graph's query (`GET /assets`), shared by the hook and its tests. */
export function assetsQuery() {
  return queryOptions({
    queryKey: ['assets'],
    queryFn: api.assets,
    staleTime: 5_000,
    // The graph rarely changes; a failed fetch is asked again until the server answers.
    refetchInterval: recovering(),
    retry: false,
  })
}

/**
 * Fetch the asset graph from barca-server. No placeholder: until the server answers, the pages
 * show their loading state, never an invented graph.
 */
export function useAssets() {
  return useQuery(assetsQuery())
}
