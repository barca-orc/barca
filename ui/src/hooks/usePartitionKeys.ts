import { useQueries, useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { KEYS_PER_NODE, type KeyHits } from '@/lib/palette'

/** A page of a partitioned node's keys (`GET /assets/{id}/partitions`), while a panel shows it. */
export function usePartitionKeys(id: string, filter: { status?: 'failed'; q?: string; limit: number }) {
  return useQuery({
    queryKey: ['partition-keys', id, filter],
    queryFn: () => api.partitionKeys(id, filter),
    refetchInterval: 10_000,
    placeholderData: (previous) => previous,
    retry: false,
  })
}

/**
 * The keys matching `q` across `ids` (the palette's key search): one request per node, in
 * parallel, each answering with at most `KEYS_PER_NODE` keys. Nothing is asked while `q` is
 * empty; `searching` is true while any node has not answered yet, and `answeredAt` is when the
 * newest answer arrived (the clock the ages are read against).
 */
export function usePartitionKeySearch(ids: readonly string[], q: string) {
  return useQueries({
    queries: ids.map((id) => ({
      queryKey: ['partition-keys', id, { q, limit: KEYS_PER_NODE }],
      queryFn: () => api.partitionKeys(id, { q, limit: KEYS_PER_NODE }),
      enabled: q !== '',
      staleTime: 10_000,
      retry: false,
    })),
    combine: (results) => ({
      hits: results.flatMap((r, i): KeyHits[] => (r.data && ids[i] !== undefined ? [{ node: ids[i], keys: r.data.keys }] : [])),
      searching: results.some((r) => r.isLoading),
      answeredAt: Math.max(0, ...results.map((r) => r.dataUpdatedAt)),
    }),
  })
}
