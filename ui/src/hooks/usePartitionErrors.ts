import { useMemo } from 'react'
import { useQueries, type UseQueryResult } from '@tanstack/react-query'
import { api } from '@/lib/api'
import type { PartitionKeys } from '@/lib/types'

const ONE_FAILED = { status: 'failed', limit: 1 } as const

const pagesOf = (results: UseQueryResult<PartitionKeys>[]) => results.map((r) => r.data)

/**
 * The failed keys of each of `ids` (`GET /assets/{id}/partitions?status=failed`: one key, and
 * the error groups over all of them), by id, as each answer arrives; the failures list heads a
 * partitioned node with them.
 */
export function usePartitionErrors(ids: readonly string[]): ReadonlyMap<string, PartitionKeys> {
  const pages = useQueries({
    queries: ids.map((id) => ({
      queryKey: ['partition-keys', id, ONE_FAILED],
      queryFn: () => api.partitionKeys(id, ONE_FAILED),
      refetchInterval: 10_000,
      retry: false,
    })),
    combine: pagesOf,
  })
  return useMemo(() => new Map(ids.flatMap((id, i) => (pages[i] ? [[id, pages[i]] as const] : []))), [ids, pages])
}
