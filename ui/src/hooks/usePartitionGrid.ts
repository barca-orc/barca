import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/**
 * Every key of a partitioned node, while the key explorer shows it. The server keeps the grid until
 * its database changes and answers a repeat with `304`, so polling it costs little.
 */
export function usePartitionGrid(id: string | null) {
  return useQuery({
    queryKey: ['partition-grid', id],
    queryFn: () => api.partitionGrid(id!),
    enabled: id !== null,
    refetchInterval: recovering(5_000),
    placeholderData: (previous, query) => (query?.queryKey[1] === id ? previous : undefined),
    retry: false,
  })
}

/** One key of a partitioned node, across its chain, with what it read and wrote. */
export function usePartitionKey(id: string | null, key: string | null) {
  return useQuery({
    queryKey: ['partition-key', id, key],
    queryFn: () => api.partitionKey(id!, key!),
    enabled: id !== null && key !== null,
    refetchInterval: recovering(10_000),
    retry: false,
  })
}
