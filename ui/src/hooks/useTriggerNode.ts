import { useCallback, useState } from 'react'
import { useTriggerGet } from '@/hooks/useTriggerGet'
import { useTriggerRun } from '@/hooks/useTriggerRun'
import { useRunDetail } from '@/hooks/useRuns'
import { runNotice, type RunNotice } from '@/lib/runFeedback'
import type { ListNode } from '@/lib/types'

type Target = Pick<ListNode, 'id' | 'kind'>

/**
 * The barca command behind a node's Run button: `run` a task (always re-executes), `get`
 * an asset or sensor (cache-aware). No "materialize".
 *
 * The run it starts is followed to its outcome (`notice`): `GET /runs/{id}` is polled while it
 * is live, and the rows it touched are refreshed when it ends (`useRunDetail`).
 */
export function useTriggerNode(
  asset: Target | null,
  onTrigger: (handle: string, nodeId: string) => void,
) {
  const getTrigger = useTriggerGet()
  const runTrigger = useTriggerRun()
  const [started, setStarted] = useState<{ runId: string; target: string | null } | null>(null)
  const [asked, setAsked] = useState<string | null>(null)
  const detail = useRunDetail(started?.runId ?? null)
  const { mutate: get, reset: resetGet } = getTrigger
  const { mutate: run, reset: resetRun } = runTrigger

  /** Start `node`, whichever is selected: a failures list retries any of its rows. */
  const fireFor = useCallback((node: Target) => {
    const mutate = node.kind === 'task' ? run : get
    setAsked(node.id)
    mutate(node.id, {
      onSuccess: (data) => {
        setStarted({ runId: data.run_id, target: node.id })
        onTrigger(data.run_id, node.id)
      },
    })
  }, [get, run, onTrigger])

  const fire = useCallback(() => {
    if (asset) fireFor(asset)
  }, [asset, fireFor])

  /** Follow a run started elsewhere (the topbar's pipeline run) in the same line. */
  const follow = useCallback((runId: string, target: string | null = null) => {
    setStarted({ runId, target })
  }, [])

  const dismiss = useCallback(() => {
    setStarted(null)
    resetGet()
    resetRun()
  }, [resetGet, resetRun])

  const trigger = asset?.kind === 'task' ? runTrigger : getTrigger
  const pending = getTrigger.isPending || runTrigger.isPending
  const failed = getTrigger.error ?? runTrigger.error
  const notice: RunNotice | null = runNotice({
    target: asked,
    pending,
    error: failed ? (failed as Error).message : null,
    started,
    run: detail.data ?? null,
  })

  return {
    fire,
    fireFor,
    follow,
    dismiss,
    notice,
    isPending: trigger.isPending,
    /** The trigger request itself failing (network/404), not a run that failed. */
    error: trigger.isError ? (trigger.error as Error) : null,
  }
}
