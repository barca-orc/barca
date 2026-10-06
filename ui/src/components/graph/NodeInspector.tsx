import { X, Download, Play, Terminal, CircleAlert } from 'lucide-react'
import { match } from 'ts-pattern'
import { Button, IconButton, StatusBadge, Tag, StatusDot, LogViewer } from '@/components'
import { useTriggerGet } from '@/hooks/useTriggerGet'
import { useTriggerRun } from '@/hooks/useTriggerRun'
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
  /** Fired when a get/run is triggered, with the run handle + target node id. */
  onTrigger: (handle: string, nodeId: string) => void
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
  onTrigger,
  onClose,
}: NodeInspectorProps) {
  const getTrigger = useTriggerGet()
  const runTrigger = useTriggerRun()
  const name = shortName(asset.id)

  // Canonical barca verbs: `run` a task (always re-executes), `get` an asset or
  // sensor (cache-aware). No "materialize".
  const isTask = asset.kind === 'task'
  const trigger = isTask ? runTrigger : getTrigger
  const verb = isTask ? 'run' : 'get'

  const onFire = () => {
    trigger.mutate(name, {
      onSuccess: (data) => onTrigger(data.run_id, asset.id),
    })
  }

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
    <div className="barca-inspector">
      <div className="barca-insp-head">
        <div className="barca-insp-title">
          <StatusDot status={status} size={8} />
          <span>{name}</span>
        </div>
        <IconButton label="Close" size="sm" onClick={onClose}>
          <X size={14} />
        </IconButton>
      </div>

      <div className="barca-insp-body">
        <div className="barca-tagrow">
          <Tag tone="signal" dot>
            {asset.kind}
          </Tag>
          <StatusBadge status={status} size="sm" />
        </div>

        <div className="barca-insp-kv">
          <div className="barca-insp-kv-row">
            <span>id</span>
            <span>{asset.id}</span>
          </div>
          <div className="barca-insp-kv-row">
            <span>kind</span>
            <span>{asset.kind}</span>
          </div>
          <div className="barca-insp-kv-row">
            <span>freshness</span>
            <span>{freshnessLabel(asset.freshness)}</span>
          </div>
          <div className="barca-insp-kv-row">
            <span>inputs</span>
            <span>{asset.inputs.length}</span>
          </div>
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
            loading={trigger.isPending || running}
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
        {trigger.isError && (
          <ErrorPanel title={`could not start ${verb}`} message={(trigger.error as Error).message} />
        )}
      </div>
    </div>
  )
}
