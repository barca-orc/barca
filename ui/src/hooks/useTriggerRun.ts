import { useMutation } from '@tanstack/react-query'
import { api } from '@/lib/api'

/**
 * Trigger a task run (POST /run/{target}) — the canonical verb for tasks, which
 * always re-execute (never cached). Returns the run handle.
 */
export function useTriggerRun() {
  return useMutation({
    mutationFn: (target: string) => api.runTarget(target),
  })
}
