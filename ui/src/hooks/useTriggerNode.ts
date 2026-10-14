import { useCallback } from 'react'
import { useTriggerGet } from '@/hooks/useTriggerGet'
import { useTriggerRun } from '@/hooks/useTriggerRun'
import type { AssetSummary } from '@/lib/types'

/**
 * The canonical barca verb for a node: `run` a task (always re-executes), `get`
 * an asset or sensor (cache-aware). No "materialize".
 */
export function useTriggerNode(
  asset: AssetSummary | null,
  onTrigger: (handle: string, nodeId: string) => void,
) {
  const getTrigger = useTriggerGet()
  const runTrigger = useTriggerRun()
  const isTask = asset?.kind === 'task'
  const trigger = isTask ? runTrigger : getTrigger
  const mutate = trigger.mutate

  const fire = useCallback(() => {
    if (!asset) return
    mutate(asset.id, { onSuccess: (data) => onTrigger(data.run_id, asset.id) })
  }, [asset, mutate, onTrigger])

  return {
    verb: isTask ? ('run' as const) : ('get' as const),
    fire,
    isPending: trigger.isPending,
    /** The trigger request itself failing (network/404), not a run that failed. */
    error: trigger.isError ? (trigger.error as Error) : null,
  }
}
