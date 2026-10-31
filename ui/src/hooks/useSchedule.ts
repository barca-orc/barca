import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/** The scheduler's job registry (`GET /schedule`): which jobs are paused. */
export function useSchedule() {
  return useQuery({
    queryKey: ['schedule'],
    queryFn: api.schedule,
    refetchInterval: recovering(10_000),
    retry: false,
  })
}

/** Pause (`POST /schedule/{id}/pause`) or resume (`.../resume`) one schedule. */
export function useToggleSchedulePause() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ id, pause }: { id: string; pause: boolean }) =>
      pause ? api.pauseSchedule(id) : api.resumeSchedule(id),
    onSettled: () => {
      void client.invalidateQueries({ queryKey: ['schedule'] })
      void client.invalidateQueries({ queryKey: ['state'] })
    },
  })
}
