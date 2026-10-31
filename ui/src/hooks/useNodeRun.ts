import { useCallback, useEffect, useMemo, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useTriggerNode } from './useTriggerNode'
import { useRunStream } from './useRunStream'
import { useTopbarRun } from './useTopbarRun'
import { overlayRunStatus, shortName } from '@/lib/graph'
import { liveGraphState, type GraphState } from '@/lib/graphState'
import type { ListNode } from '@/lib/types'

/** One action shared by the selected node's panel and the page's topbar. */
export interface NodeRunAction {
  fire: () => void
  triggering: boolean
  readOnly: boolean
  title: string
  error: Error | null
  liveState?: GraphState
}

export function useNodeRun(node: Pick<ListNode, 'id' | 'kind'> | null, { showTopbar = true, scopes = true } = {}) {
  const [run, setRun] = useState<{ handle: string; nodeId: string | null } | null>(null)
  const onTrigger = useCallback((handle: string, nodeId: string) => setRun({ handle, nodeId }), [])
  const trigger = useTriggerNode(node, onTrigger)
  const stream = useRunStream(run?.handle ?? null)
  const client = useQueryClient()
  const { fire, fireFor, follow, dismiss, notice } = trigger
  const triggering = trigger.isPending || stream.running
  const onStarted = useCallback((handle: string) => {
    setRun({ handle, nodeId: null })
    follow(handle)
  }, [follow])
  const topbar = useTopbarRun({ node, show: showTopbar, scopes, fireNode: fire, busy: triggering, onStarted })
  const { readOnly } = topbar
  const title = readOnly ? topbar.title : node ? `Run ${shortName(node.id)}` : topbar.title

  useEffect(() => {
    if (!run || stream.running) return
    void client.invalidateQueries({ queryKey: ['state'] })
    void client.invalidateQueries({ queryKey: ['asset'] })
  }, [run, stream.running, client])

  // Seed the accepted target as running before its first SSE event, just as GraphPage does.
  const statuses = useMemo(
    () => stream.running ? overlayRunStatus(stream.statuses, true, run?.nodeId ?? null) : {},
    [stream.statuses, stream.running, run?.nodeId],
  )
  const liveStatus = node ? statuses[node.id] : undefined
  const action: NodeRunAction = {
    fire, triggering, readOnly, title, error: trigger.error ?? topbar.error,
    liveState: liveStatus ? liveGraphState(liveStatus) : undefined,
  }
  return {
    action, run, statuses, stream,
    /** The run this page started, followed to its outcome (`RunNotice` shows it). */
    notice, dismiss,
    /** Start any node, not only the selected one (a failures list's Retry). */
    retry: fireFor,
  }
}
