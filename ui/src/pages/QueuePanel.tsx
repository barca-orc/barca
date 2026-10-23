import { Link } from 'react-router'
import { match } from 'ts-pattern'
import { Button, StatusDot, Tag } from '@/components'
import { useQueue, useToggleQueuePause } from '@/hooks/useQueue'
import { ApiError } from '@/lib/api'
import { capacityLabel, queueControl, queuedLabel, queueRows, queueState, queueStateLabel, sharedCapacityLabel, waitingRows } from '@/lib/queue'

/**
 * The run queue on the Runs page: how many run of the server's limit, how many wait, whether
 * the queue is paused or draining, the operator's pause control, and the waiting runs in order.
 * The strip keeps its height while the queue loads, so the run table below it does not move.
 */
export function QueuePanel({ readOnly }: { readOnly: boolean | undefined }) {
  const { data, isError, error } = useQueue()
  const toggle = useToggleQueuePause()
  const state = data ? queueState(data) : null
  const stateLabel = state ? queueStateLabel(state) : null
  const control = data ? queueControl(data, readOnly) : { kind: 'hidden' as const }
  const rows = data ? queueRows(data.queued) : []
  const shared = data ? sharedCapacityLabel(data.capacity) : null
  const waiting = data ? waitingRows(data.capacity) : []
  const toggleError = toggle.error instanceof ApiError ? toggle.error : null
  return (
    <section className="barca-queue" aria-label="Queue">
      <div className="barca-queue-bar">
        <span className="barca-queue-title">Queue</span>
        {!data && (isError
          ? <span className="barca-note" role="alert">Queue unavailable: {error.message}</span>
          : <span className="barca-note">Loading queue…</span>)}
        {data && <>
          <span className="barca-queue-count">
            <StatusDot status={data.running.length > 0 ? 'running' : 'pending'} size={6} />
            {capacityLabel(data.running.length, data.max_runs)}
          </span>
          <span className="barca-queue-count">{queuedLabel(data.queued.length)}</span>
          {shared && <span className="barca-queue-count" title="The capacity every run of this server shares (--capacity)">{shared}</span>}
          {state && stateLabel && match(state)
            .with({ kind: 'open' }, () => null)
            .with({ kind: 'paused' }, () => <Tag tone="bare">{stateLabel}</Tag>)
            .with({ kind: 'draining' }, () => <Tag>{stateLabel}</Tag>)
            .exhaustive()}
        </>}
        <span className="barca-queue-actions">
          {readOnly && <Tag tone="bare">read-only</Tag>}
          {match(control)
            .with({ kind: 'hidden' }, () => null)
            .with({ kind: 'pause' }, { kind: 'resume' }, (c) => (
              <Button size="sm" variant="neutral" loading={toggle.isPending} disabled={c.disabled || toggle.isPending}
                onClick={() => toggle.mutate(c.kind === 'pause')}>
                {c.kind === 'pause' ? 'Pause' : 'Resume'}
              </Button>
            ))
            .exhaustive()}
        </span>
      </div>
      {toggle.isError && (
        <p className="barca-note barca-queue-error" role="alert">
          Can't {toggle.variables ? 'pause' : 'resume'} the queue: {toggle.error.message}
          {toggleError?.remediation && <> {toggleError.remediation}</>}
        </p>
      )}
      {waiting.length > 0 && (
        <table className="barca-table barca-queue-table" aria-label="Steps waiting for capacity">
          <thead><tr><th className="num">#</th><th>Waiting step</th><th>Run</th><th className="num">Weight</th><th>Waiting</th></tr></thead>
          <tbody>{waiting.map(row => (
            <tr key={row.runId}>
              <td className="num">{row.position}</td>
              <td><code className="barca-run-target" title={row.node}>{row.node}</code></td>
              <td><Link className="barca-run-link" to={row.href}>{row.runId}</Link></td>
              <td className="num">{row.weight}</td>
              <td>{row.waited}</td>
            </tr>
          ))}</tbody>
        </table>
      )}
      {rows.length > 0 && (
        <table className="barca-table barca-queue-table">
          <thead><tr><th className="num">#</th><th>Run</th><th>Target</th><th>Trigger</th><th>Queued</th></tr></thead>
          <tbody>{rows.map(row => (
            <tr key={row.runId}>
              <td className="num">{row.position}</td>
              <td><Link className="barca-run-link" to={row.href}>{row.runId}</Link><div className="barca-cell-sub">{row.command}</div></td>
              <td><code className="barca-run-target" title={row.target}>{row.target}</code></td>
              <td>{row.trigger}{row.scheduleId && <div className="barca-cell-sub">{row.scheduleId}</div>}</td>
              <td>{row.queuedAt}</td>
            </tr>
          ))}</tbody>
        </table>
      )}
    </section>
  )
}
