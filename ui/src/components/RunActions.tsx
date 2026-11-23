import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useWriteControl } from '@/hooks/useWriteControl'
import { api } from '@/lib/api'
import { rerunAllRequest, rerunFailedRequest, retryStepRequest } from '@/lib/runRecovery'
import type { RunRequest, RunView } from '@/lib/types'
import { ConfirmDialog } from './ConfirmDialog'
import { Button } from './Button'

type Rerunnable = Pick<RunView, 'command' | 'target' | 'files'>

/** Start `request`, then show the new run: `onStarted` gets its id. One request at a time. */
function useStart(onStarted: (runId: string) => void) {
  const client = useQueryClient()
  const writes = useWriteControl()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const start = async (request: RunRequest) => {
    setBusy(true)
    setError(null)
    try {
      const { run_id } = await api.startRun(request)
      void client.refetchQueries({ queryKey: ['runs', 'list'] })
      onStarted(run_id)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }
  return { start, busy, error, writes }
}

/**
 * A finished run's recovery: run it again cache-aware (the steps that succeeded are cached, so
 * only the failed steps and what they stopped run), or with every step recomputed. None on a
 * read-only server.
 */
export function RunActions({ run, retryLabel, onStarted }: { run: Rerunnable; retryLabel: string | null; onStarted: (runId: string) => void }) {
  const { start, busy, error, writes } = useStart(onStarted)
  if (writes === 'hidden') return null
  const off = busy || writes === 'disabled'
  return <div className="barca-run-actions" style={{ display: 'contents' }}>
    {retryLabel && <Button size="sm" variant="signal" loading={busy} disabled={off} title="Run the same targets again using cached results where available"
      onClick={() => void start(rerunFailedRequest(run))}>{retryLabel}</Button>}
    <Button size="sm" disabled={off} title="Re-run the whole run: the same target with every step recomputed (--refresh-all)"
      onClick={() => void start(rerunAllRequest(run))}>Re-run all</Button>
    {error && <p className="barca-run-error" role="alert">Could not start the run: {error}</p>}
  </div>
}

/** "Retry" of one failed step (one key of a partitioned node: only that key). None on a read-only server. */
export function RetryStep({ run, stepId, onStarted }: { run: Rerunnable; stepId: string; onStarted: (runId: string) => void }) {
  const { start, busy, error, writes } = useStart(onStarted)
  if (writes === 'hidden') return null
  return <>
    <Button size="sm" variant="ghost" loading={busy} disabled={busy || writes === 'disabled'} title={`Run ${stepId} again`}
      onClick={(e) => { e.stopPropagation(); void start(retryStepRequest(run, stepId)) }}>Retry</Button>
    {error && <span className="barca-run-error" role="alert">{error}</span>}
  </>
}

/** Cancel one live run, keeping the request visible until polling reports its terminal state. */
export function CancelRun({ id, queued, control }: { id: string; queued: boolean; control: RunView['control'] }) {
  const client = useQueryClient()
  const writes = useWriteControl()
  const [asking, setAsking] = useState(false)
  const cancel = useMutation({
    mutationFn: () => api.cancelRun(id),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['runs'] })
      void client.invalidateQueries({ queryKey: ['queue'] })
    },
  })
  if (!control.cancellable) return <span className="barca-note">{control.reason ?? `Run owned by ${control.owner}`}</span>
  // Hidden until the server is known to take writes: a cancel is never offered to be refused.
  if (writes !== 'enabled') return null
  return <>
    <Button size="sm" disabled={cancel.isPending || cancel.isSuccess} onClick={() => setAsking(true)}>
      {cancel.isSuccess ? 'Cancellation requested' : cancel.isPending ? 'Cancelling…' : 'Cancel run'}
    </Button>
    {cancel.isError && <span role="alert">Could not cancel: {cancel.error.message}</span>}
    {asking && <ConfirmDialog
      title={`Cancel run ${id}?`}
      body={queued ? 'Remove this run from the queue so it does not start.' : 'Stop this run and its active work. Results already completed stay recorded.'}
      action="Cancel run"
      onCancel={() => setAsking(false)}
      onConfirm={() => { setAsking(false); cancel.mutate() }}
    />}
  </>
}
