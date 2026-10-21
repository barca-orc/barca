import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Trigger a task run (`POST /runs` with
 * `{"command":"run","targets":[target],"refresh_all":true}`) — the canonical
 * verb for tasks, which always re-execute along with their upstream assets.
 * Returns the run id.
 */
export function useTriggerRun() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (target: string) => api.runTarget(target),
    onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ['runs'] }) },
  })
}
