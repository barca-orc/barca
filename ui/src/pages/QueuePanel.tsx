import { useState } from 'react'
import { Link } from 'react-router'
import { ChevronDown, ChevronRight } from 'lucide-react'
import { match } from 'ts-pattern'
import { Age, Button, IconButton, StatusBadge, StatusGlyph, Tag } from '@/components'
import { useQueue, useToggleQueuePause } from '@/hooks/useQueue'
import { useTimeZone } from '@/context/timeZone'
import { ApiError } from '@/lib/api'
import { runTime } from '@/lib/runs'
import { capacityLabel, queueControl, queueMatters, queuedLabel, queueRows, queueState, queueStateLabel, queueSummary, sharedCapacityLabel, waitingRows } from '@/lib/queue'
import type { QueueView } from '@/lib/types'

/** Where the reader's choice to fold the queue's list is kept (this browser only). */
const COLLAPSED_KEY = 'barca.runs.queue-collapsed'

function readCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSED_KEY) === '1'
  } catch {
    return false
  }
}

function writeCollapsed(collapsed: boolean) {
  try {
    localStorage.setItem(COLLAPSED_KEY, collapsed ? '1' : '0')
  } catch {
    // Storage blocked (a private window): the choice lasts this page only.
  }
}

/**
 * The run queue on the Runs page (`QueuePanelView` with the server's queue): its data, the
 * pause control's request, and whether the reader folded the list.
 */
export function QueuePanel({ readOnly, startedAt }: { readOnly: boolean | undefined; startedAt?: (runId: string) => string | null }) {
  const { data, isError, error, refetch, dataUpdatedAt } = useQueue()
  const toggle = useToggleQueuePause()
  const [collapsed, setCollapsed] = useState(readCollapsed)
  const toggleError = toggle.isError ? { action: toggle.variables ? 'pause' : 'resume', message: toggle.error.message, remediation: toggle.error instanceof ApiError ? toggle.error.remediation : null } : null
  return (
    <QueuePanelView
      queue={data}
      loadError={!data && isError ? error.message : null}
      onRetry={() => void refetch()}
      readOnly={readOnly}
      pending={toggle.isPending}
      toggleError={toggleError}
      onPause={(pause) => toggle.mutate(pause)}
      collapsed={collapsed}
      onCollapse={(next) => {
        setCollapsed(next)
        writeCollapsed(next)
      }}
      nowMs={dataUpdatedAt}
      startedAt={startedAt}
    />
  )
}

export interface QueuePanelViewProps {
  /** `GET /queue`; undefined until it loads. */
  queue: QueueView | undefined
  /** Why the queue could not load, while there is none to show. */
  loadError: string | null
  onRetry: () => void
  readOnly: boolean | undefined
  /** A pause or resume is in flight. */
  pending: boolean
  /** The last pause or resume that failed. */
  toggleError: { action: string; message: string; remediation: string | null } | null
  onPause: (pause: boolean) => void
  /** The list is folded to the bar and a summary line. */
  collapsed: boolean
  onCollapse: (collapsed: boolean) => void
  nowMs: number
  /** A running run's start, from the run list; its time counts from it. */
  startedAt?: (runId: string) => string | null
}

/**
 * The run queue: how many run of the server's limit, how many wait, whether the queue is
 * paused or draining, the operator's pause control, and the runs, the running ones first.
 * Idle, it is not there at all: it appears when something runs or waits, or the queue holds work
 * back, so a paused queue can still be resumed. Its list never takes more than its share of the
 * page (it scrolls), and folds to one summary line, so the run history keeps the page.
 */
export function QueuePanelView({ queue: data, loadError, onRetry, readOnly, pending, toggleError, onPause, collapsed, onCollapse, nowMs, startedAt }: QueuePanelViewProps) {
  const { zone } = useTimeZone()
  const state = data ? queueState(data) : null
  const formatTime = (value: string | null) => runTime(value, zone)
  const stateLabel = state ? queueStateLabel(state, formatTime) : null
  const control = data ? queueControl(data, readOnly) : { kind: 'hidden' as const }
  // The Queued cell draws its time with `Age`: the row keeps it as the server wrote it.
  const rows = data ? queueRows(data, (value) => value ?? '', nowMs, startedAt) : []
  const shared = data ? sharedCapacityLabel(data.capacity) : null
  const waiting = data ? waitingRows(data.capacity) : []
  const summary = data ? queueSummary(data, nowMs) : null
  const listed = rows.length + waiting.length
  if (!toggleError && (data ? !queueMatters(data) : loadError === null)) return null
  return (
    <section className="barca-queue" aria-label="Queue" data-collapsed={collapsed || undefined}>
      <div className="barca-queue-bar">
        {listed > 0
          ? <IconButton size="sm" label={collapsed ? 'Show the queued runs' : 'Hide the queued runs'} aria-expanded={!collapsed} onClick={() => onCollapse(!collapsed)}>
            {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
          </IconButton>
          : null}
        <span className="barca-queue-title">Queue</span>
        {!data && loadError !== null && <span className="barca-note" role="alert">Queue unavailable: {loadError} <Button size="sm" onClick={onRetry}>Retry</Button></span>}
        {data && <>
          <span className="barca-queue-count">
            <StatusGlyph glyph={data.running.length > 0 ? 'running' : 'queued'} live={data.running.length > 0} size={12} />
            {capacityLabel(data.running.length, data.max_runs)}
          </span>
          <span className="barca-queue-count">{queuedLabel(data.queued.length)}</span>
          {shared && <span className="barca-queue-count" title="The capacity every run of this server shares (--capacity)">{shared}</span>}
          {state && stateLabel && match(state)
            .with({ kind: 'open' }, () => null)
            .with({ kind: 'paused' }, () => <Tag tone="bare">{stateLabel}</Tag>)
            .with({ kind: 'draining' }, () => <Tag>{stateLabel}</Tag>)
            .exhaustive()}
          {collapsed && summary && <span className="barca-queue-summary" title={summary}>{summary}</span>}
        </>}
        <span className="barca-queue-actions">
          {match(control)
            .with({ kind: 'hidden' }, () => null)
            .with({ kind: 'pause' }, { kind: 'resume' }, (c) => (
              <Button size="sm" variant="neutral" loading={pending} disabled={c.disabled || pending}
                onClick={() => onPause(c.kind === 'pause')}>
                {c.kind === 'pause' ? 'Pause' : 'Resume'}
              </Button>
            ))
            .exhaustive()}
        </span>
      </div>
      {toggleError && (
        <p className="barca-note barca-queue-error" role="alert">
          Can't {toggleError.action} the queue: {toggleError.message}
          {toggleError.remediation && <> {toggleError.remediation}</>}
        </p>
      )}
      {!collapsed && listed > 0 && <div className="barca-queue-list">
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
          <table className="barca-table barca-queue-table barca-queue-runs" aria-label="Queued runs">
            <thead><tr><th>Place</th><th>Run</th><th>Target</th><th>Trigger</th><th>Queued</th><th className="num" title="How long it has run, or waited to start">Elapsed</th></tr></thead>
            <tbody>{rows.map(row => (
              <tr key={row.runId} data-state={row.state}>
                <td>{row.state === 'running'
                  ? <StatusBadge size="sm" glyph="running" label="running" live />
                  : <span className="barca-queue-place">{row.position}</span>}</td>
                <td><Link className="barca-run-link" to={row.href}>{row.runId}</Link> <span className="barca-cell-sub">{row.command}</span></td>
                <td><code className="barca-run-target" title={row.target}>{row.target}</code></td>
                <td>{row.trigger}{row.scheduleId && <span className="barca-cell-sub"> {row.scheduleId}</span>}</td>
                <td>{row.queuedAt ? <Age at={row.queuedAt} nowMs={nowMs} /> : '–'}</td>
                <td className="num" title={row.state === 'running' ? 'Running for' : 'Waiting for'}>{row.elapsed}</td>
              </tr>
            ))}</tbody>
          </table>
        )}
      </div>}
    </section>
  )
}
