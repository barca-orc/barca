import { X, Play, Terminal } from 'lucide-react'
import { match } from 'ts-pattern'
import { Button, ErrorLine, IconButton, KeyValue, SidePanel, StatusBadge, StatusGlyph, LogViewer } from '@/components'
import { CopyButton } from '@/components/assets/NodeKeys'
import { isLive, type GraphState } from '@/lib/graphState'
import { freshnessLabel, nodeKindLabel } from '@/lib/status'
import { shortName } from '@/lib/graph'
import { runFeedback } from '@/lib/runFeedback'
import type { ListNode, StatusKind, LogLine } from '@/lib/types'

interface NodeInspectorProps {
  asset: ListNode
  /** The node's box: its cache state in the CLI's word, or the run working on it. */
  state: GraphState
  /** The streamed run's status for this node: what feedback to show. */
  feedbackStatus: StatusKind
  /** Captured log lines for the active run (run-wide). */
  logs: LogLine[]
  /** Whether a run is currently streaming. */
  running: boolean
  /** Failure message for this node, if the last run failed. */
  error: string | null
  /** The server refuses runs (`barca serve --read-only`). */
  readOnly: boolean
  /** The node's trigger state (owned by the page, shared with the topbar). */
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
        <StatusGlyph glyph="failed" size={12} />
        <span>{title}</span>
      </div>
      <ErrorLine error={message} large />
    </div>
  )
}

export function NodeInspector({
  asset,
  state,
  feedbackStatus,
  logs,
  running,
  error,
  readOnly,
  triggering,
  triggerError,
  onFire,
  onClose,
}: NodeInspectorProps) {
  const name = shortName(asset.id)

  // Presentation logic (pure, tested) decides the feedback descriptor; this
  // component only maps each descriptor variant to elements. Exhaustive on both
  // sides — no run state can render nothing by accident.
  const feedback = match(runFeedback(feedbackStatus, logs, error))
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
      titleText={name}
      // The same header as the node panel: the name, its kind in words, the id with its copy, Run.
      badge={<span className="barca-sidepanel-kind">{nodeKindLabel(asset)}</span>}
      subtitle={asset.id}
      subtitleText={asset.id}
      subtitleAction={<CopyButton label="Copy id" text={asset.id} />}
      actions={
        <>
          <Button
            variant="signal"
            size="sm"
            iconLeft={<Play size={12} />}
            loading={triggering || running}
            disabled={readOnly}
            title={readOnly ? 'This server is read-only' : undefined}
            onClick={onFire}
          >
            Run
          </Button>
          <IconButton label="Close" size="sm" onClick={onClose}>
            <X size={14} />
          </IconButton>
        </>
      }
    >
      <div className="barca-insp-body">
        <div className="barca-tagrow">
          <StatusBadge glyph={state.glyph} label={state.label} live={isLive(state)} />
        </div>

        <div>
          <KeyValue label="freshness">{freshnessLabel(asset)}</KeyValue>
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

        {feedback}

        {/* Failure of the trigger request itself (network/404), distinct from a
            run that started and then failed. */}
        {triggerError && (
          <ErrorPanel title="could not start the run" message={triggerError.message} />
        )}
      </div>
    </SidePanel>
  )
}
