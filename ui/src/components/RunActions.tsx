import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useHealth } from '@/hooks/useHealth'
import { api } from '@/lib/api'
import { rerunAllRequest, rerunFailedRequest, retryStepRequest } from '@/lib/runRecovery'
import type { RunRequest, RunView } from '@/lib/types'
import { ConfirmDialog } from './ConfirmDialog'
import { Button } from './Button'

type Rerunnable = Pick<RunView, 'command' | 'target' | 'files'>

/** Start `request`, then show the new run: `onStarted` gets its id. One request at a time. */
function useStart(onStarted: (runId: string) => void) {
  const client = useQueryClient()
  const readOnly = useHealth().data?.read_only === true
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
  return { start, busy, error, readOnly }
}

const READ_ONLY = 'This server is read-only: it starts no runs.'

/**
 * A finished run's recovery: run it again cache-aware (the steps that succeeded are cached, so
 * only the failed steps and what they stopped run), or with every step recomputed.
 */
export function RunActions({ run, failed, onStarted }: { run: Rerunnable; failed: boolean; onStarted: (runId: string) => void }) {
  const { start, busy, error, readOnly } = useStart(onStarted)
  return <div className="barca-run-actions" style={{ display: 'contents' }}>
    {failed && <Button size="sm" variant="signal" loading={busy} disabled={busy || readOnly} title={readOnly ? READ_ONLY : 'Re-run the failed steps and everything downstream of them: the same target again, the steps that succeeded are cached'}
      onClick={() => void start(rerunFailedRequest(run))}>Re-run failed</Button>}
    <Button size="sm" disabled={busy || readOnly} title={readOnly ? READ_ONLY : 'Re-run the whole run: the same target with every step recomputed (--refresh-all)'}
      onClick={() => void start(rerunAllRequest(run))}>Re-run all</Button>
    {error && <p className="barca-run-error" role="alert">Could not start the run: {error}</p>}
  </div>
}

/** "Retry" of one failed step (one key of a partitioned node: only that key). */
export function RetryStep({ run, stepId, onStarted }: { run: Rerunnable; stepId: string; onStarted: (runId: string) => void }) {
  const { start, busy, error, readOnly } = useStart(onStarted)
  return <>
    <Button size="sm" variant="ghost" loading={busy} disabled={busy || readOnly} title={readOnly ? READ_ONLY : `Run ${stepId} again`}
      onClick={(e) => { e.stopPropagation(); void start(retryStepRequest(run, stepId)) }}>Retry</Button>
    {error && <span className="barca-run-error" role="alert">{error}</span>}
  </>
}

/** Cancel one live run, keeping the request visible until polling reports its terminal state. */
export function CancelRun({ id, queued }: { id: string; queued: boolean }) {
  const client = useQueryClient()
  const readOnly = useHealth().data?.read_only ?? true
  const [asking, setAsking] = useState(false)
  const cancel = useMutation({
    mutationFn: () => api.cancelRun(id),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['runs'] })
      void client.invalidateQueries({ queryKey: ['queue'] })
    },
  })
  if (readOnly) return null
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
