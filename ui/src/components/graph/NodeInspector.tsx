import { X, Download, Play, Terminal, CircleAlert } from 'lucide-react'
import { match } from 'ts-pattern'
import { Button, IconButton, KeyValue, SidePanel, StatusBadge, Tag, StatusDot, LogViewer } from '@/components'
import { freshnessLabel } from '@/lib/status'
import { shortName } from '@/lib/graph'
import { runFeedback } from '@/lib/runFeedback'
import type { AssetSummary, StatusKind, LogLine } from '@/lib/types'

interface NodeInspectorProps {
  asset: AssetSummary
  /** Live visual status for this node. */
  status: StatusKind
  /** Captured log lines for the active run (run-wide). */
  logs: LogLine[]
  /** Whether a run is currently streaming. */
  running: boolean
  /** Failure message for this node, if the last run failed. */
  error: string | null
  /** The server refuses runs (`barca serve --read-only`). */
  readOnly: boolean
  /** The node's trigger verb and state (owned by the page, shared with the topbar). */
  verb: 'run' | 'get'
  triggering: boolean
  triggerError: Error | null
  onFire: () => void
  onClose: () => void
}

/** Output (log) panel — the live/streaming and completed-with-output surface. */
function OutputPanel({ logs, live }: { logs: LogLine[]; live: boolean }) {
  return (
    <div className="barca-insp-logs">
      <div className="barca-insp-logs-head">
        <Terminal size={12} />
        <span>output</span>
        {live && <span className="barca-insp-logs-live">live</span>}
      </div>
      <LogViewer lines={logs} live={live} height={220} />
    </div>
  )
}

/** Failure callout — surfaces a run/step error message. */
function ErrorPanel({ title, message }: { title: string; message: string }) {
  return (
    <div className="barca-insp-error">
      <div className="barca-insp-error-head">
        <CircleAlert size={13} />
        <span>{title}</span>
      </div>
      <div className="barca-insp-error-msg">{message}</div>
    </div>
  )
}

export function NodeInspector({
  asset,
  status,
  logs,
  running,
  error,
  readOnly,
  verb,
  triggering,
  triggerError,
  onFire,
  onClose,
}: NodeInspectorProps) {
  const name = shortName(asset.id)
  const isTask = asset.kind === 'task'

  // Presentation logic (pure, tested) decides the feedback descriptor; this
  // component only maps each descriptor variant to elements. Exhaustive on both
  // sides — no run state can render nothing by accident.
  const feedback = match(runFeedback(status, logs, error))
    .with({ kind: 'idle' }, () => null)
    .with({ kind: 'streaming' }, (f) => <OutputPanel logs={f.logs} live />)
    .with({ kind: 'output' }, (f) => <OutputPanel logs={f.logs} live={false} />)
    .with({ kind: 'done' }, () => <div className="barca-insp-note">done · no output</div>)
    .with({ kind: 'failed' }, (f) => (
      <>
        <ErrorPanel title="run failed" message={f.error ?? 'unknown error'} />
        {f.logs.length > 0 && <OutputPanel logs={f.logs} live={false} />}
      </>
    ))
    .exhaustive()

  return (
    <SidePanel
      label={`${name} inspector`}
      width={316}
      title={name}
      badge={<StatusDot status={status} size={8} />}
      actions={
        <IconButton label="Close" size="sm" onClick={onClose}>
          <X size={14} />
        </IconButton>
      }
    >
      <div className="barca-insp-body">
        <div className="barca-tagrow">
          <Tag tone="signal" dot>
            {asset.kind}
          </Tag>
          <StatusBadge status={status} size="sm" />
        </div>

        <div>
          <KeyValue label="id">{asset.id}</KeyValue>
          <KeyValue label="kind">{asset.kind}</KeyValue>
          <KeyValue label="freshness">{freshnessLabel(asset.freshness)}</KeyValue>
          <KeyValue label="inputs">{asset.inputs.length}</KeyValue>
        </div>

        {asset.inputs.length > 0 && (
          <div className="barca-insp-deps">
            {asset.inputs.map((input) => (
              <span className="barca-insp-dep" key={input}>
                {shortName(input)}
              </span>
            ))}
          </div>
        )}

        <div className="barca-insp-actions">
          <Button
            variant="signal"
            size="sm"
            iconLeft={isTask ? <Play size={12} /> : <Download size={12} />}
            loading={triggering || running}
            disabled={readOnly}
            title={readOnly ? 'This server is read-only' : undefined}
            onClick={onFire}
          >
            {verb}
          </Button>
          <Button variant="ghost" size="sm" iconLeft={<Terminal size={13} />}>
            logs
          </Button>
        </div>

        {feedback}

        {/* Failure of the trigger request itself (network/404), distinct from a
            run that started and then failed. */}
        {triggerError && (
          <ErrorPanel title={`could not start ${verb}`} message={triggerError.message} />
        )}
      </div>
    </SidePanel>
  )
}
