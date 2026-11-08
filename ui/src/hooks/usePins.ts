import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { pinNotes } from '@/lib/pins'
import { recovering } from './polling'

/** The pinned assets (`GET /pins`), shared by every view that shows a pinned node. */
export function usePins() {
  return useQuery({
    queryKey: ['pins'],
    queryFn: api.pins,
    refetchInterval: recovering(10_000),
    retry: false,
  })
}

/** Each pinned node id and what it is pinned to, in a line (`override from fix.json by X`). */
export function usePinNotes(): ReadonlyMap<string, string> {
  const { data } = usePins()
  return useMemo(() => pinNotes(data), [data])
}
