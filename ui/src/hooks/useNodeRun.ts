import { useCallback, useEffect, useMemo, useState } from 'react'
import { useOutletContext } from 'react-router'
import { useQueryClient } from '@tanstack/react-query'
import { useTriggerNode } from './useTriggerNode'
import { useRunStream } from './useRunStream'
import { useHealth } from './useHealth'
import { overlayRunStatus, shortName } from '@/lib/graph'
import { liveGraphState, type GraphState } from '@/lib/graphState'
import type { AppShellContext } from '@/layouts/shellContext'
import type { ListNode } from '@/lib/types'

/** One action shared by the selected node's panel and the page's topbar. */
export interface NodeRunAction {
  fire: () => void
  verb: 'get' | 'run'
  triggering: boolean
  readOnly: boolean
  title: string
  error: Error | null
  liveState?: GraphState
}

export function useNodeRun(node: Pick<ListNode, 'id' | 'kind'> | null, showTopbar = true) {
  const [run, setRun] = useState<{ handle: string; nodeId: string } | null>(null)
  const onTrigger = useCallback((handle: string, nodeId: string) => setRun({ handle, nodeId }), [])
  const trigger = useTriggerNode(node, onTrigger)
  const stream = useRunStream(run?.handle ?? null)
  const { data: health } = useHealth()
  const readOnly = health?.read_only ?? false
  const { setTopbarRun } = useOutletContext<AppShellContext>()
  const client = useQueryClient()
  const { fire, verb } = trigger
  const triggering = trigger.isPending || stream.running
  const name = node ? shortName(node.id) : null
  const title = readOnly ? 'This server is read-only' : name ? `${verb} ${name}` : 'Select a node to run'

  useEffect(() => {
    if (!showTopbar) return
    setTopbarRun({ onRun: fire, disabled: !name || readOnly, loading: triggering, title })
    return () => setTopbarRun(null)
  }, [showTopbar, setTopbarRun, fire, name, readOnly, triggering, title])

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
    fire, verb, triggering, readOnly, title, error: trigger.error,
    liveState: liveStatus ? liveGraphState(liveStatus) : undefined,
  }
  return { action, run, statuses }
}
