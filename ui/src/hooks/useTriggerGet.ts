import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Trigger a cache-aware materialization of a single target (POST /get/{target}).
 * Returns the run handle; status polling/streaming is wired separately.
 */
export function useTriggerGet() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (target: string) => api.getTarget(target),
    onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ['runs'] }) },
  })
}
