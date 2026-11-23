import { Link } from 'react-router'
import { plural } from '@/lib/plural'
import { X } from 'lucide-react'
import { match } from 'ts-pattern'
import { shortName } from '@/lib/graph'
import type { RunNotice as Notice } from '@/lib/runFeedback'
import { statusFailed, statusMeta } from '@/lib/status'
import { CancelRun } from './RunActions'
import { IconButton } from './IconButton'
import { StatusGlyph } from './StatusGlyph'

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
    .with({ kind: 'queued' }, (n) => (n.restart ? 'queued · starts after the server restarts' : 'queued'))
    .with({ kind: 'running' }, (n) => `running · ${n.total === null ? plural(n.done, 'step') : `${n.done.toLocaleString()} of ${plural(n.total, 'step')}`}`)
    .with({ kind: 'finished' }, (n) => n.summary)
    .exhaustive()
  const failed = notice.kind === 'rejected' || (notice.kind === 'finished' && statusFailed(notice.status))
  return (
    <p role={failed ? 'alert' : 'status'} className="barca-note barca-run-notice">
      <StatusGlyph glyph={statusMeta(status).glyph} live={statusMeta(status).live} size={12} />
      {name && <strong title={notice.target ?? undefined}>{name}</strong>}
      {'runId' in notice && (
        <>
          <span>·</span>
          <Link to={`/runs?run=${encodeURIComponent(notice.runId)}`}>run {notice.runId}</Link>
        </>
      )}
      <span>·</span>
      <span>{text}</span>
      {match(notice)
        .with({ kind: 'queued' }, n => <CancelRun id={n.runId} queued control={n.control} />)
        .with({ kind: 'running' }, n => <CancelRun id={n.runId} queued={false} control={n.control} />)
        .otherwise(() => null)}
      <IconButton size="sm" label="Dismiss" onClick={onDismiss}>
        <X size={12} />
      </IconButton>
    </p>
  )
}
