import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Trigger a task run (`POST /runs` with
 * `{"command":"run","targets":[target]}`) — the canonical
 * verb for tasks, which always execute while their upstream assets use the cache.
 * Returns the run id.
 */
export function useTriggerRun() {
  const queryClient = useQueryClient()
  return useMutation({
    /** `{ target, allowPaused }` runs a paused target, after the operator confirmed it. */
    mutationFn: (target: string | { target: string; allowPaused: boolean }) =>
      typeof target === 'string' ? api.runTarget(target) : api.runTarget(target.target, target.allowPaused),
    onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ['runs'] }) },
  })
}
