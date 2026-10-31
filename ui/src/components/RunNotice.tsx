import { Link } from 'react-router'
import { X } from 'lucide-react'
import { match } from 'ts-pattern'
import { shortName } from '@/lib/graph'
import type { RunNotice as Notice } from '@/lib/runFeedback'
import { IconButton } from './IconButton'
import { StatusDot } from './StatusDot'

/**
 * The run a page started, in one line: what was asked for, the run id (a link to its page),
 * and where it is, from queued to its outcome. Stays until dismissed, so the outcome is read.
 */
export function RunNotice({ notice, onDismiss }: { notice: Notice | null; onDismiss: () => void }) {
  if (!notice) return null
  const name = notice.target ? shortName(notice.target) : null
  const status = match(notice)
    .with({ kind: 'starting' }, { kind: 'queued' }, () => 'queued' as const)
    .with({ kind: 'running' }, () => 'running' as const)
    .with({ kind: 'rejected' }, () => 'failed' as const)
    .with({ kind: 'finished' }, (n) => n.status)
    .exhaustive()
  const text = match(notice)
    .with({ kind: 'starting' }, () => 'starting…')
    .with({ kind: 'rejected' }, (n) => `could not start: ${n.error}`)
    .with({ kind: 'queued' }, () => 'queued')
    .with({ kind: 'running' }, (n) => `running · ${n.done.toLocaleString()}${n.total === null ? '' : ` of ${n.total.toLocaleString()}`} steps`)
    .with({ kind: 'finished' }, (n) => n.summary)
    .exhaustive()
  const failed = notice.kind === 'rejected' || (notice.kind === 'finished' && notice.status === 'failed')
  return (
    <p
      role={failed ? 'alert' : 'status'}
      className="barca-note barca-run-notice"
      style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '8px 24px', margin: 0 }}
    >
      <StatusDot status={status} size={6} />
      {name && <strong title={notice.target ?? undefined}>{name}</strong>}
      {'runId' in notice && (
        <>
          <span>·</span>
          <Link to={`/runs?run=${encodeURIComponent(notice.runId)}`}>run {notice.runId}</Link>
        </>
      )}
      <span>·</span>
      <span>{text}</span>
      <IconButton size="sm" label="Dismiss" onClick={onDismiss}>
        <X size={12} />
      </IconButton>
    </p>
  )
}
