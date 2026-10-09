import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Trigger a task run (POST /run/{target}) — the canonical verb for tasks, which
 * always re-execute (never cached). Returns the run handle.
 */
export function useTriggerRun() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (target: string) => api.runTarget(target),
    onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ['runs'] }) },
  })
}
