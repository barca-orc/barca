import { useMutation } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Trigger a cache-aware materialization of a single target (POST /get/{target}).
 * Returns the run handle; status polling/streaming is wired separately.
 */
export function useTriggerGet() {
  return useMutation({
    mutationFn: (target: string) => api.getTarget(target),
  })
}
