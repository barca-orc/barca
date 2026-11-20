import { useMemo } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { pausedIds } from '@/lib/pauses'
import type { PauseRequest } from '@/lib/types'
import { recovering } from './polling'

/** The paused assets (`GET /pauses`). */
export function usePauses() {
  return useQuery({
    queryKey: ['pauses'],
    queryFn: api.pauses,
    refetchInterval: recovering(10_000),
    retry: false,
  })
}

/**
 * What a pause of `id` and everything below it would cover (`GET /pauses?target=`, `barca pause
 * NODE --downstream --dry-run`): the nodes below it and the schedules whose ticks reach them, by
 * the scheduler's own walk. It depends on the graph alone, so it is kept until the page reloads
 * the graph.
 */
export function usePauseCover(id: string) {
  return useQuery({
    queryKey: ['pauses', 'cover', id],
    queryFn: () => api.pauseCover(id),
    select: (pauses) => pauses.covers,
    staleTime: 60_000,
    retry: false,
  })
}

/** Pause (`POST /pauses`) or resume (`POST /pauses/resume`) assets. Both answer every pause. */
export function useTogglePause() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ pause, ...body }: PauseRequest & { pause: boolean }) =>
      pause ? api.pauseAssets(body) : api.resumeAssets(body),
    onSuccess: (pauses) => client.setQueryData(['pauses'], pauses),
    onSettled: () => { void client.invalidateQueries({ queryKey: ['pauses'] }) },
  })
}

/** The ids of the paused assets, from `GET /pauses`. */
export function usePausedSet(): ReadonlySet<string> {
  const { data } = usePauses()
  return useMemo(() => pausedIds(data), [data])
}
