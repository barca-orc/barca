import { useState, type ReactNode } from 'react'
import { X } from 'lucide-react'
import { match } from 'ts-pattern'
import { Age, Button, Chip, ChipGroup, ErrorLine, IconButton, KeyValue, LogViewer, SearchInput, Section, SidePanel, Skeleton, StatusBadge } from '@/components'
import { formatSeconds } from '@/lib/assetTable'
import type { Dispatch } from '@/lib/program'
import type { Remote } from '@/lib/remote'
import { shortName } from '@/lib/graph'
import { outcomeResult, parseOutcome, runIsLive, runLabel, runStatus, runTime, triggeredByLabel } from '@/lib/runs'
import { activityBarLabel, activityTicks, canLoadMore, groupRunSteps, hourLabel, runActivity, runNodeLine, runFailed, runFailureLine, runProgress, runRowError, visibleRuns, type RunActivity, type RunFilter, type RunsModel, type RunsMsg } from '@/lib/runsPage'
import type { RunNodeGroup } from '@/lib/runsPage'
import type { RunList, RunStep, RunView } from '@/lib/types'
import './runs.css'

/** One run's details, for any state of its request. */
function StepList({ steps }: { steps: readonly RunStep[] }) {
  return <ul className="barca-run-steps">
    {steps.map((step, index) => <li key={`${step.node_id}:${index}`}>
      <code>{step.node_id}</code>
      <div><StatusBadge size="sm" status={step.status === 'cached' ? 'skipped' : runStatus(step.status)} label={step.status} />
        {step.elapsed_seconds !== null && <span>{formatSeconds(step.elapsed_seconds)}</span>}</div>
      {step.error && <p className="barca-run-error">{step.error}</p>}
    </li>)}
  </ul>
}

/** One node of a run with partitioned steps; its keys render only once it is opened. */
function RunNodeRow({ group: { summary, steps } }: { group: RunNodeGroup }) {
  const [open, setOpen] = useState(summary.failed > 0)
  const hidden = summary.keys - steps.length
  return <details className="barca-run-node" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
    <summary>
      <code>{shortName(summary.node_id)}</code>
      <StatusBadge size="sm" status={summary.failed ? 'failed' : 'success'} label={runNodeLine(summary)} />
      <span>{formatSeconds(summary.elapsed_seconds)}</span>
    </summary>
    {open && <StepList steps={steps} />}
    {open && hidden > 0 && <p className="barca-note">{hidden.toLocaleString()} more keys not listed here.</p>}
  </details>
}

export function RunPanelView({ id, run, eventGap = false, dispatch }: { id: string; run: Remote<RunView>; eventGap?: boolean; dispatch: Dispatch<RunsMsg> }) {
  const retry = <Button size="sm" onClick={() => dispatch({ type: 'retry_detail' })}>Retry</Button>
  return (
    <SidePanel label="Run details" title="Run details" subtitle={run.kind === 'ready' ? run.data.run_id : id}
      actions={<IconButton label="Close run details" size="sm" onClick={() => dispatch({ type: 'panel_closed' })}><X size={14} /></IconButton>}>
      {match(run)
        .with({ kind: 'loading' }, () => <Skeleton height={180} />)
        .with({ kind: 'failed' }, ({ error }) => (
          <Section title="Run unavailable">
            <p role="alert">{error}</p>
            {retry}
          </Section>
        ))
        .with({ kind: 'ready' }, ({ data, refreshError }) => <>
          {refreshError && <Section title="Updates unavailable">
            <p role="alert">Showing the last loaded run information: {refreshError}</p>
            {retry}
          </Section>}
          <RunDetails data={data} eventGap={eventGap} />
        </>)
        .exhaustive()}
    </SidePanel>
  )
}

function RunDetails({ data, eventGap }: { data: RunView; eventGap: boolean }) {
  const outcome = parseOutcome(data.outcome)
  const envelope = outcome && outcome.status !== 'success' ? outcome.envelope : null
  const result = outcomeResult(outcome)
  return <>
    <Section title="Run">
      <StatusBadge status={runStatus(data.status)} label={runLabel(data.status)} />
      <KeyValue label="id"><code className="barca-wrap">{data.run_id}</code></KeyValue>
      <KeyValue label="command">{data.command}</KeyValue>
      <KeyValue label="target"><code className="barca-wrap">{data.target ?? 'all assets'}</code></KeyValue>
      <KeyValue label="triggered by">{triggeredByLabel(data.triggered_by)}</KeyValue>
      <KeyValue label="started">{runTime(data.started_at)}</KeyValue>
      <KeyValue label="finished">{runTime(data.finished_at)}</KeyValue>
      <KeyValue label="duration">{data.elapsed_seconds === null ? '–' : formatSeconds(data.elapsed_seconds)}</KeyValue>
      <KeyValue label="executed">{data.steps_executed}</KeyValue>
      <KeyValue label="cached">{data.steps_cached}</KeyValue>
      {(data.error ?? envelope?.error) && <p className="barca-run-error" role="alert"><ErrorLine error={data.error ?? envelope?.error} /></p>}
      {envelope?.remediation && <p className="barca-note">{envelope.remediation}</p>}
    </Section>
    <Section title="Steps">
      {data.steps.length === 0 ? <p className="barca-note">{runIsLive(data.status) ? 'Waiting for steps…' : 'No step results recorded.'}</p> : (
        data.steps_page?.nodes?.length
          ? <div className="barca-run-nodes">{groupRunSteps(data.steps, data.steps_page.nodes).map((group) => <RunNodeRow key={group.summary.node_id} group={group} />)}</div>
          : <StepList steps={data.steps} />
      )}
      {data.steps_page?.truncated && <p className="barca-note">Showing {data.steps.length} of {data.steps_page.total} steps; failed steps are always listed. barca history {data.run_id} --all lists every one.</p>}
    </Section>
    <Section title="Logs">
      {eventGap && runIsLive(data.status) && <p role="status" className="barca-note">Live event replay was incomplete. These logs may show only a retained tail until the run finishes.</p>}
      <LogViewer lines={data.logs.map(log => ({ nodeId: log.node_id, text: log.line, stream: log.stream, ts: log.ts }))} live={runIsLive(data.status)} height={280} />
    </Section>
    {result !== null && result !== undefined && (
      <Section title="Result"><pre className="barca-run-result">{JSON.stringify(result, null, 2)}</pre></Section>
    )}
  </>
}

export interface RunsViewProps {
  model: RunsModel
  runs: Remote<RunList>
  /** `?run=`, with its details; null when no run is selected. */
  selected: { id: string; run: Remote<RunView>; eventGap?: boolean } | null
  /** The queue strip (`QueuePanel`, which loads its own data). */
  queue?: ReactNode
  /** When the list was loaded (ms since epoch): the activity strip's last hour. */
  now: number
  dispatch: Dispatch<RunsMsg>
}

/** The Runs page, for any model, run list and selection: renders and dispatches, nothing else. */
const FILTERS: { value: RunFilter; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'failed', label: 'Not successful' },
  { value: 'live', label: 'In progress' },
]

/** The last day hour by hour: a bar per hour, failures in red on top; a bar filters to its hour. */
function ActivityStrip({ activity, hour, now, dispatch }: { activity: RunActivity; hour: number | null; now: number; dispatch: Dispatch<RunsMsg> }) {
  const peak = Math.max(1, ...activity.hours.map((h) => h.ok + h.failed + h.live))
  const width = 100 / activity.hours.length
  const nowAt = 100 - width + (width * ((now % 3_600_000) / 3_600_000))
  return (
    <div className="barca-activity" aria-label="Runs started in the last 24 hours, hour by hour">
      <div className="barca-activity-chart">
        <div className="barca-activity-bars">
          {activity.hours.map((h) => {
            const n = h.ok + h.failed + h.live
            const label = activityBarLabel(h)
            return (
              <button key={h.at} type="button" className="barca-activity-bar" title={label} aria-label={label} aria-pressed={hour === h.at} disabled={n === 0}
                onClick={() => dispatch({ type: 'hour_toggled', at: h.at })}>
                <span style={{ height: `${Math.max(n ? 8 : 2, (n / peak) * 100)}%` }}>
                  {h.failed > 0 && <i data-kind="failed" style={{ flexGrow: h.failed }} />}
                  {h.live > 0 && <i data-kind="live" style={{ flexGrow: h.live }} />}
                  {h.ok > 0 && <i data-kind="ok" style={{ flexGrow: h.ok }} />}
                </span>
              </button>
            )
          })}
        </div>
        <div className="barca-activity-axis" aria-hidden="true">
          {activityTicks(activity.hours).map((t) => <span key={t.index} style={{ left: `${t.index * width}%` }}>{t.label}</span>)}
          <span className="barca-activity-now" style={{ left: `${nowAt}%` }}>now</span>
        </div>
      </div>
      <div className="barca-activity-summary">
        <strong>{activity.total.toLocaleString()}</strong> runs in the last 24 h
        {activity.failed > 0 && <> · <span className="barca-cell-failed">{activity.failed} not successful ({Math.round((activity.failed / activity.total) * 100)}%)</span></>}
        {activity.medianSeconds !== null && <> · median {formatSeconds(activity.medianSeconds)}</>}
        {activity.partial && <> · of the runs loaded</>}
      </div>
    </div>
  )
}

export function RunsView({ model, runs, selected, queue, now, dispatch }: RunsViewProps) {
  const list = runs.kind === 'ready' ? runs.data : null
  return (
    <div className="barca-view">
      <div className="barca-view-head"><div className="barca-view-bar">
        <div className="barca-view-title"><h1>Runs</h1>{list && <span className="barca-count">{list.total} runs</span>}</div>
      </div></div>
      {queue}
      <div className="barca-assets-split">
        <div className="barca-view-body barca-table-scroll">
          {match(runs)
            .with({ kind: 'loading' }, () => <div aria-label="Loading runs"><Skeleton height={180} /></div>)
            .with({ kind: 'failed' }, ({ error }) => <RunsError error={error} dispatch={dispatch} />)
            .with({ kind: 'ready' }, ({ data, refreshError }) => {
              const shown = visibleRuns(data.runs, model)
              const failedCount = data.runs.filter((r) => runFailed(r.status)).length
              return <>
              {refreshError && <RunsError error={refreshError} dispatch={dispatch} />}
              {data.runs.length > 0 && <>
                <ActivityStrip activity={runActivity(data.runs, now, 24, data.truncated)} hour={model.hour} now={now} dispatch={dispatch} />
                <div className="barca-table-tools">
                  <ChipGroup label="Show">
                    {FILTERS.map((f) => (
                      <Chip key={f.value} pressed={model.filter === f.value} onClick={() => dispatch({ type: 'filter_changed', filter: f.value })}>
                        {f.label}{f.value === 'failed' ? ` · ${failedCount}` : ''}
                      </Chip>
                    ))}
                  </ChipGroup>
                  <SearchInput placeholder="Filter by target or run id" value={model.query} onChange={(value) => dispatch({ type: 'query_changed', value })} />
                  {model.hour !== null && <Chip pressed onClick={() => dispatch({ type: 'hour_toggled', at: model.hour! })}>{hourLabel(model.hour)}–{hourLabel(model.hour + 3_600_000)} <X size={12} /></Chip>}
                  <span className="barca-count">{shown.length.toLocaleString()} shown</span>
                </div>
              </>}
              {data.runs.length === 0
                ? <p className="barca-table-empty">No runs recorded. Start a run from the graph to see it here.</p>
                : shown.length === 0
                  ? <p className="barca-table-empty">No loaded run matches.</p>
                  : <RunTable runs={shown} selected={selected?.id ?? null} now={now} dispatch={dispatch} />}
              {data.truncated && <div className="barca-run-more">
                <p className="barca-note">Showing the latest {data.runs.length} runs.</p>
                {canLoadMore(model) && <Button size="sm" onClick={() => dispatch({ type: 'load_more' })}>Load more</Button>}
              </div>}
            </>
            })
            .exhaustive()}
        </div>
        {selected && <RunPanelView key={selected.id} id={selected.id} run={selected.run} eventGap={selected.eventGap} dispatch={dispatch} />}
      </div>
    </div>
  )
}

function RunsError({ error, dispatch }: { error: string; dispatch: Dispatch<RunsMsg> }) {
  return <p className="barca-table-empty" role="alert">Can't load runs: {error} <Button size="sm" onClick={() => dispatch({ type: 'retry_list' })}>Retry</Button></p>
}

function RunTable({ runs, selected, now, dispatch }: { runs: RunList['runs']; selected: string | null; now: number; dispatch: Dispatch<RunsMsg> }) {
  return <table className="barca-table barca-run-table"><thead><tr>
    <th>Run</th><th>Target</th><th>Trigger</th><th>Status</th><th>Started</th><th className="num">Duration</th><th className="num">Steps</th><th>Failure</th>
  </tr></thead><tbody>{runs.map(run => {
    const error = runRowError(run)
    const progress = runProgress(run)
    return <tr key={run.id} aria-selected={selected !== null && (selected === run.id || selected === run.handle)}>
    <td><button className="barca-run-link" onClick={() => dispatch({ type: 'run_selected', id: run.id })}>{run.id}</button>{run.command !== 'get' && <span className="barca-cell-sub"> {run.command}</span>}</td>
    <td><code className="barca-run-target" title={run.target ?? undefined}>{run.target ?? 'all assets'}</code></td>
    <td>{triggeredByLabel(run.triggered_by)}</td>
    <td><StatusBadge size="sm" status={runStatus(run.status)} label={runLabel(run.status)} /></td>
    <td><Age at={run.started_at} nowMs={now} /></td>
    <td className="num">{run.elapsed_seconds === null ? '–' : formatSeconds(run.elapsed_seconds)}</td>
    <td className="num barca-run-steps-cell" title={progress.title}>{progress.steps}</td>
    <td className="barca-run-failure">{error && <ErrorLine error={error.title} text={runFailureLine(run) ?? error.text} />}</td>
  </tr>
  })}</tbody></table>
}
