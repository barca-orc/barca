import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

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
