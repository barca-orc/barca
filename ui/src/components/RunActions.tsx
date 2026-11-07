import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useHealth } from '@/hooks/useHealth'
import { api } from '@/lib/api'
import { rerunAllRequest, rerunFailedRequest, retryStepRequest } from '@/lib/runRecovery'
import type { RunRequest, RunView } from '@/lib/types'
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
    {failed && <Button size="sm" variant="signal" loading={busy} disabled={busy || readOnly} title={readOnly ? READ_ONLY : 'The same target again: cached steps are skipped, so the failed steps and their downstream run'}
      onClick={() => void start(rerunFailedRequest(run))}>Re-run failed steps + downstream</Button>}
    <Button size="sm" disabled={busy || readOnly} title={readOnly ? READ_ONLY : 'The same target with every step recomputed (--refresh-all)'}
      onClick={() => void start(rerunAllRequest(run))}>Re-run whole run</Button>
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
