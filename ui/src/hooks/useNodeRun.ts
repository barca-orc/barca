import { runNodeTitle } from '@/lib/runTarget'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useTriggerNode } from './useTriggerNode'
import { useRunStream } from './useRunStream'
import { useTopbarRun } from './useTopbarRun'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { liveGraphState, type GraphState } from '@/lib/graphState'
import { partitionSelection } from '@/lib/nodeDetail'
import type { ListNode } from '@/lib/types'
import type { WriteControl } from '@/lib/writeAccess'

/** One action shared by the selected node's panel and the command palette. */
export interface NodeRunAction {
  /** `allowPaused`: run a paused node, after the operator confirmed it (`--allow-paused`). */
  fire: (allowPaused?: boolean) => void
  /** `barca get <node> --partition <key>`: one key of a partitioned node, across its cone. */
  fireKey: (key: string) => void
  includeManual: boolean
  setIncludeManual: (value: boolean) => void
  hardRefresh: (allowPaused?: boolean) => void
  /** The key whose run is being started, while the request is in flight. */
  startingKey: string | null
  triggering: boolean
  /** How its Run (and the panel's other writes) are drawn: hidden on a read-only server (`lib/writeAccess.ts`). */
  writes: WriteControl
  /** The server is read-only: the panel offers no writes. */
  readOnly: boolean
  title: string
  error: Error | null
  liveState?: GraphState
}

export function useNodeRun(node: Pick<ListNode, 'id' | 'kind'> | null, { showTopbar = true, scopes = true } = {}) {
  const [run, setRun] = useState<{ handle: string; nodeId: string | null } | null>(null)
  const onTrigger = useCallback((handle: string, nodeId: string) => setRun({ handle, nodeId }), [])
  const [includeManual, setIncludeManual] = useState(false)
  const options = useMemo(() => includeManual ? { include_manual: true } : {}, [includeManual])
  const trigger = useTriggerNode(node, onTrigger, options)
  const stream = useRunStream(run?.handle ?? null)
  const client = useQueryClient()
  const { fire, fireFor, follow, dismiss, notice } = trigger
  const keyRun = useMutation({
    mutationFn: ({ id, key }: { id: string; key: string }) =>
      api.startRun({ command: 'get', targets: [id], partition: partitionSelection(key), ...options }),
    onSuccess: (data, { id }) => {
      setRun({ handle: data.run_id, nodeId: id })
      follow(data.run_id, id)
      void client.invalidateQueries({ queryKey: ['runs'] })
    },
  })
  const { mutate: mutateKey } = keyRun
  const fireKey = useCallback((key: string) => { if (node) mutateKey({ id: node.id, key }) }, [node, mutateKey])
  const hardRefresh = useCallback((allowPaused = false) => {
    if (node) fireFor(node, allowPaused, { refresh_all: true })
  }, [node, fireFor])
  const triggering = trigger.isPending || keyRun.isPending || stream.running
  const onStarted = useCallback((handle: string) => {
    setRun({ handle, nodeId: null })
    follow(handle)
  }, [follow])
  const topbar = useTopbarRun({ node, show: showTopbar, scopes, fireNode: fire, busy: triggering, onStarted })
  const { readOnly, writes } = topbar
  const title = writes === 'disabled' ? topbar.title : node ? runNodeTitle(shortName(node.id), includeManual) : topbar.title

  useEffect(() => {
    if (!run || stream.running) return
    void client.invalidateQueries({ queryKey: ['state'] })
    void client.invalidateQueries({ queryKey: ['rollup'] })
    void client.invalidateQueries({ queryKey: ['asset'] })
    void client.invalidateQueries({ queryKey: ['partition-keys'] })
  }, [run, stream.running, client])

  // A run was accepted: how many nodes of a scope a run works on is the server's count (the
  // rollup's `running`), so it is asked again at once instead of being counted here.
  useEffect(() => {
    if (run) void client.invalidateQueries({ queryKey: ['rollup'] })
  }, [run, client])

  // An accepted run's target reads `queued` until the run reports on it: a worker holding a
  // step is what `running` means (issue 1042), and a click still has to show that it was taken.
  const target = run?.nodeId ?? null
  const statuses = useMemo(() => {
    if (!stream.running) return {}
    return target && !(target in stream.statuses) ? { ...stream.statuses, [target]: 'queued' as const } : stream.statuses
  }, [stream.running, stream.statuses, target])
  const liveStatus = node ? statuses[node.id] : undefined
  const action: NodeRunAction = {
    fire, fireKey, includeManual, setIncludeManual, hardRefresh, startingKey: keyRun.isPending ? keyRun.variables.key : null,
    triggering, writes, readOnly, title, error: trigger.error ?? (keyRun.error as Error | null) ?? topbar.error,
    liveState: liveStatus ? liveGraphState(liveStatus) : undefined,
  }
  return {
    action, run, statuses, stream,
    /** The run this page started, followed to its outcome (`RunNotice` shows it). */
    notice, dismiss,
    /** Start any node, not only the selected one (a failures list's Retry). */
    retry: fireFor,
    /** The toolbar's Run: what is open, the pipeline or the project (`PageRunButton`). */
    scopeRun: topbar.scopeRun,
  }
}
