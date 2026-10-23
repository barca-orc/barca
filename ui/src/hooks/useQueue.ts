import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { queuePollInterval } from '@/lib/queue'

/** The run queue (`GET /queue`): polled faster while anything runs or waits. */
export function useQueue() {
  return useQuery({
    queryKey: ['queue'],
    queryFn: api.queue,
    refetchInterval: (q) => queuePollInterval(q.state.data),
    retry: false,
  })
}

/** Pause (`POST /queue/pause`) or resume (`POST /queue/resume`) the queue. Both answer the queue. */
export function useToggleQueuePause() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (pause: boolean) => (pause ? api.pauseQueue() : api.resumeQueue()),
    onSuccess: (view) => client.setQueryData(['queue'], view),
    onSettled: () => {
      void client.invalidateQueries({ queryKey: ['queue'] })
      void client.invalidateQueries({ queryKey: ['health'] })
      void client.invalidateQueries({ queryKey: ['runs', 'list'] })
    },
  })
}
