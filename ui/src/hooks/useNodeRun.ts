import { useCallback, useEffect, useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useTriggerNode } from './useTriggerNode'
import { useRunStream } from './useRunStream'
import { useTopbarRun } from './useTopbarRun'
import { api } from '@/lib/api'
import { overlayRunStatus, shortName } from '@/lib/graph'
import { liveGraphState, type GraphState } from '@/lib/graphState'
import { partitionSelection } from '@/lib/nodeDetail'
import type { ListNode } from '@/lib/types'

/** One action shared by the selected node's panel and the page's topbar. */
export interface NodeRunAction {
  fire: () => void
  /** `barca get <node> --partition <key>`: one key of a partitioned node, across its cone. */
  fireKey: (key: string) => void
  /** The key whose run is being started, while the request is in flight. */
  startingKey: string | null
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
  const keyRun = useMutation({
    mutationFn: ({ id, key }: { id: string; key: string }) =>
      api.startRun({ command: 'get', targets: [id], partition: partitionSelection(key) }),
    onSuccess: (data, { id }) => {
      setRun({ handle: data.run_id, nodeId: id })
      void client.invalidateQueries({ queryKey: ['runs'] })
    },
  })
  const { fire } = trigger
  const { mutate: mutateKey } = keyRun
  const fireKey = useCallback((key: string) => { if (node) mutateKey({ id: node.id, key }) }, [node, mutateKey])
  const triggering = trigger.isPending || keyRun.isPending || stream.running
  const onStarted = useCallback((handle: string) => setRun({ handle, nodeId: null }), [])
  const topbar = useTopbarRun({ node, show: showTopbar, scopes, fireNode: fire, busy: triggering, onStarted })
  const { readOnly } = topbar
  const title = readOnly ? topbar.title : node ? `Run ${shortName(node.id)}` : topbar.title

  useEffect(() => {
    if (!run || stream.running) return
    void client.invalidateQueries({ queryKey: ['state'] })
    void client.invalidateQueries({ queryKey: ['asset'] })
    void client.invalidateQueries({ queryKey: ['partition-keys'] })
  }, [run, stream.running, client])

  // Seed the accepted target as running before its first SSE event, just as GraphPage does.
  const statuses = useMemo(
    () => stream.running ? overlayRunStatus(stream.statuses, true, run?.nodeId ?? null) : {},
    [stream.statuses, stream.running, run?.nodeId],
  )
  const liveStatus = node ? statuses[node.id] : undefined
  const action: NodeRunAction = {
    fire, fireKey, startingKey: keyRun.isPending ? keyRun.variables.key : null,
    triggering, readOnly, title, error: trigger.error ?? (keyRun.error as Error | null) ?? topbar.error,
    liveState: liveStatus ? liveGraphState(liveStatus) : undefined,
  }
  return { action, run, statuses, stream }
}
