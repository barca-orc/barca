import { useMemo } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { pausedIds, type PauseRequest } from '@/lib/pauses'
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
