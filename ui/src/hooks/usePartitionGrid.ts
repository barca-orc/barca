import { useEffect, useRef } from 'react'
import { useInfiniteQuery, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/** Keys asked of the server at a time; the table asks for the next page as it is scrolled to. */
export const KEYS_PAGE = 200

/**
 * A partitioned node's keys in one picture, while the key explorer shows it. The server keeps
 * the node's grid until its database changes and answers a repeat with `304`, so polling it
 * costs little.
 */
export function usePartitionOverview(id: string | null) {
  return useQuery({
    queryKey: ['partition-overview', id],
    queryFn: () => api.partitionOverview(id!),
    enabled: id !== null,
    refetchInterval: recovering(5_000),
    placeholderData: (previous, query) => (query?.queryKey[1] === id ? previous : undefined),
    retry: false,
  })
}

/**
 * The keys of `id` that match `query` (the route's parameters, `keysQuery`), a page at a time:
 * the server filters, orders and counts them, so a node with 100,000 keys costs the browser one
 * page. `stamp` is what the overview last said of the node; the pages are asked for again when
 * it changes, so the table follows a run without polling every page it has loaded.
 */
export function usePartitionKeysPages(id: string | null, query: string | null, stamp: unknown) {
  const client = useQueryClient()
  const result = useInfiniteQuery({
    queryKey: ['partition-keys-page', id, query],
    queryFn: ({ pageParam }) => api.partitionKeysPage(id!, query!, { limit: KEYS_PAGE, offset: pageParam }),
    initialPageParam: 0,
    getNextPageParam: (last) => (last.truncated ? last.offset + last.keys.length : undefined),
    enabled: id !== null && query !== null,
    placeholderData: (previous) => previous,
    retry: false,
  })
  const seen = useRef(stamp)
  useEffect(() => {
    if (seen.current === stamp) return
    seen.current = stamp
    void client.invalidateQueries({ queryKey: ['partition-keys-page', id] })
  }, [client, id, stamp])
  return result
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
