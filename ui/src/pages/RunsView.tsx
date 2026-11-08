import { Fragment, createContext, useContext, useState, type ReactNode } from 'react'
import { pathBreaks, relativeTraceback } from '@/lib/health'
import { X } from 'lucide-react'
import { match } from 'ts-pattern'
import { Absolute, Age, Button, Chip, ChipGroup, EmptyNote, ErrorLine, IconButton, LogViewer, SearchInput, Section, SidePanel, Skeleton, StatusBadge, StatusGlyph } from '@/components'
import { IncidentSummaryButton } from '@/components/IncidentSummary'
import { RetryStep, RunActions } from '@/components/RunActions'
import { useTimeZone } from '@/context/timeZone'
import { failedSteps, raisedAt, traceUrl } from '@/lib/runRecovery'
import { formatSeconds } from '@/lib/assetTable'
import { zoneName } from '@/lib/time'
import { plural } from '@/lib/plural'
import type { Dispatch } from '@/lib/program'
import type { Remote } from '@/lib/remote'
import { shortName } from '@/lib/graph'
import { outcomeResult, parseOutcome, runGlyph, runIsLive, runLabel, triggeredByLabel } from '@/lib/runs'
import { statusGlyph } from '@/lib/status'
import { activityBarLabel, activityTicks, canLoadMore, groupRunSteps, hourLabel, runActivity, runFailed, runFailureLine, runNodeIdle, runNodeOutcome, runNodeTime, runProgress, runRowError, runTargetLines, splitRunSteps, runViewSteps, visibleRuns, type RunActivity, type RunFilter, type RunsModel, type RunsMsg } from '@/lib/runsPage'
import type { RunNodeGroup } from '@/lib/runsPage'
import type { RunList, RunStep, RunView } from '@/lib/types'

/** A step's recorded error: its headline in words, the frames below it in mono, paths short and wrapping at `/`. */
function Traceback({ error, root }: { error: string; root?: string | null }) {
  const text = relativeTraceback(error.trim(), root)
  const [head, ...rest] = text.split('\n')
  return <div className="barca-run-error">
    <div className="barca-run-error-head">{head}</div>
    {rest.length > 0 && <pre className="barca-error barca-traceback">
      {pathBreaks(rest.join('\n')).map((piece, i) => <Fragment key={i}>{piece}<wbr /></Fragment>)}
    </pre>}
  </div>
}

/** How many steps a list shows before it asks to be expanded. */
const STEP_ROWS = 50

/** What a step row can do in the run panel: retry it, and filter the logs to it. */
interface StepTools {
  run: RunView
  logStep: string | null
  setLogStep: (step: string | null) => void
  onStarted: (runId: string) => void
  /** The Datadog web app a step's trace opens in; null when the server names none. */
  datadogUrl: string | null
}
const StepToolsContext = createContext<StepTools | null>(null)

/** "View trace": the step's Datadog trace in a new tab; nothing when it sent none. */
export function TraceLink({ step, datadogUrl }: { step: Pick<RunStep, 'node_id' | 'trace_id'>; datadogUrl: string | null | undefined }) {
  const href = traceUrl(datadogUrl, step)
  if (!href) return null
  return <a className="barca-btn barca-btn--ghost" data-size="sm" href={href} target="_blank" rel="noreferrer noopener"
    title={`Open ${step.node_id}'s trace in Datadog`} onClick={(e) => e.stopPropagation()}>View trace</a>
}

/** A step's "Retry" (failed steps of a finished run), "Logs" (filters the logs to it) and "View trace". */
function StepButtons({ stepId, failed, step }: { stepId: string; failed: boolean; step?: RunStep }) {
  const tools = useContext(StepToolsContext)
  if (!tools) return null
  const on = tools.logStep === stepId
  return <span className="barca-run-step-actions">
    {step && <TraceLink step={step} datadogUrl={tools.datadogUrl} />}
    <Button size="sm" variant="ghost" aria-pressed={on} title={on ? 'Show every step\'s logs' : `Show only ${stepId}'s logs`}
      onClick={(e) => { e.preventDefault(); e.stopPropagation(); tools.setLogStep(on ? null : stepId) }}>{on ? 'All logs' : 'Logs'}</Button>
    {failed && !runIsLive(tools.run.status) && <RetryStep run={tools.run} stepId={stepId} onStarted={tools.onStarted} />}
  </span>
}

/** A step's error, unless the summary at the top of the panel already says it in full. */
function StepError({ step, root, summarized }: { step: RunStep; root?: string | null; summarized: ReadonlySet<RunStep> }) {
  return step.error && !summarized.has(step) ? <Traceback error={step.error} root={root} /> : null
}

/**
 * A run's steps, at most `STEP_ROWS` until expanded. A failed step's error is said once: in the
 * summary at the top of the panel, or, past the summary's first five, here with its traceback.
 */
function StepList({ steps, root, summarized }: { steps: readonly RunStep[]; root?: string | null; summarized: ReadonlySet<RunStep> }) {
  const [all, setAll] = useState(false)
  const shown = all ? steps : steps.slice(0, STEP_ROWS)
  return <>
    <ul className="barca-run-steps">
      {shown.map((step, index) => <li key={`${step.node_id}:${index}`}>
        <code>{step.node_id}</code>
        <div><StatusBadge size="sm" glyph={step.status === 'cached' ? 'cached' : runGlyph(step.status)} label={step.status} />
          {step.elapsed_seconds !== null && <span>{formatSeconds(step.elapsed_seconds)}</span>}
          <StepButtons stepId={step.node_id} failed={step.status === 'failed'} step={step} /></div>
        <StepError step={step} root={root} summarized={summarized} />
      </li>)}
    </ul>
    {shown.length < steps.length && <Button size="sm" onClick={() => setAll(true)}>Show all {steps.length.toLocaleString()}</Button>}
  </>
}

/**
 * One planned node of a run: what became of it, and, for one that ran, its keys once opened:
 * failed keys listed at once, successful ones behind their count.
 */
function RunNodeRow({ group: { summary, steps }, root, summarized }: { group: RunNodeGroup; root?: string | null; summarized: ReadonlySet<RunStep> }) {
  const [open, setOpen] = useState(summary.failed > 0)
  const outcome = runNodeOutcome(summary)
  const time = runNodeTime(summary, formatSeconds)
  if (runNodeIdle(summary)) {
    return <div className="barca-run-node barca-run-node-idle" data-status={summary.status}>
      <code>{shortName(summary.node_id)}</code>
      <StatusBadge size="sm" glyph={statusGlyph(outcome.kind)} label={outcome.label} />
      {outcome.detail && <span className="barca-run-node-detail">{outcome.detail}</span>}
    </div>
  }
  // One key, one step: the node's row is the step's, with nothing under it to open but an error.
  const only = summary.keys <= 1 && steps.length === 1 ? steps[0]! : null
  if (only) {
    return <div className="barca-run-node barca-run-node-one" data-status={summary.status}>
      <div className="barca-run-node-line">
        <code>{shortName(summary.node_id)}</code>
        <StatusBadge size="sm" glyph={statusGlyph(outcome.kind)} label={outcome.label} />
        {time && <span className="barca-run-node-time" title={time.title}>{time.text}</span>}
        <StepButtons stepId={only.node_id} failed={only.status === 'failed'} step={only} />
      </div>
      <StepError step={only} root={root} summarized={summarized} />
    </div>
  }
  const { failed, ok } = splitRunSteps(steps)
  const hidden = summary.keys - steps.length
  return <details className="barca-run-node" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
    <summary>
      <code>{shortName(summary.node_id)}</code>
      <StatusBadge size="sm" glyph={statusGlyph(outcome.kind)} label={outcome.label} />
      {time && <span className="barca-run-node-time" title={time.title}>{time.text}</span>}
      <StepButtons stepId={summary.node_id} failed={summary.failed > 0} />
    </summary>
    {open && failed.length > 0 && <StepList steps={failed} root={root} summarized={summarized} />}
    {open && ok.length > 0 && (failed.length > 0 || summary.keys > 1
      ? <details className="barca-run-node-ok"><summary>{ok.length.toLocaleString()} successful key{ok.length === 1 ? '' : 's'}</summary><StepList steps={ok} root={root} summarized={summarized} /></details>
      : <StepList steps={ok} root={root} summarized={summarized} />)}
    {open && hidden > 0 && <p className="barca-note">{hidden.toLocaleString()} more keys not listed here.</p>}
  </details>
}

export function RunPanelView({ id, run, eventGap = false, root, datadogUrl = null, dispatch }: { id: string; run: Remote<RunView>; eventGap?: boolean; root?: string | null; datadogUrl?: string | null; dispatch: Dispatch<RunsMsg> }) {
  const retry = <Button size="sm" onClick={() => dispatch({ type: 'retry_detail' })}>Retry</Button>
  return (
    <SidePanel label="Run details" title="Run details" subtitle={run.kind === 'ready' ? run.data.run_id : id} subtitleText={run.kind === 'ready' ? run.data.run_id : id}
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
          <RunDetails data={data} eventGap={eventGap} root={root} datadogUrl={datadogUrl} onStarted={(runId) => dispatch({ type: 'run_selected', id: runId })} />
        </>)
        .exhaustive()}
    </SidePanel>
  )
}

/**
 * The top of a run's panel: for a failed run, what failed and where, then the recovery and
 * hand-off actions, so neither needs a scroll.
 */
/** How many failed steps the summary names, with their errors; the rest are in Steps. */
const SUMMARY_STEPS = 5

/** A run that failed before any step did (it could not plan, say): its own error; null otherwise. */
function runSummaryError(data: RunView): string | null {
  if (failedSteps(data).length > 0) return null
  const outcome = parseOutcome(data.outcome)
  return data.error ?? (outcome && outcome.status !== 'success' ? outcome.envelope?.error : null) ?? null
}

/** One fact of a run: a muted label over its value; `wide` takes the whole line. */
function Fact({ label, wide = false, children }: { label: string; wide?: boolean; children: ReactNode }) {
  return <div className="barca-fact" data-wide={wide || undefined}><dt>{label}</dt><dd>{children}</dd></div>
}

function RunSummary({ data, root, datadogUrl, onStarted, onFilter }: { data: RunView; root?: string | null; datadogUrl: string | null; onStarted: (runId: string) => void; onFilter: (step: string) => void }) {
  const live = runIsLive(data.status)
  const failed = failedSteps(data)
  const shown = failed.slice(0, SUMMARY_STEPS)
  // A run that failed before any step did (it could not plan, say): its own error, said here once.
  const runError = runSummaryError(data)
  return <div className="barca-run-summary">
    {failed.length > 0 && <>
      <strong className="barca-run-summary-head"><StatusGlyph glyph="failed" />{failed.length.toLocaleString()} failed step{failed.length === 1 ? '' : 's'}</strong>
      <ul className="barca-run-summary-failed">
        {shown.map((step) => {
          const where = raisedAt(step.error_traceback ?? step.error)
          return <li key={step.node_id}>
            <button type="button" className="barca-run-link" title="Show this step's logs" onClick={() => onFilter(step.node_id)}><code>{shortName(step.node_id)}</code></button>
            <ErrorLine error={step.error_traceback ?? step.error} text={step.error_type && step.error_message ? `${step.error_type}: ${step.error_message}` : undefined} root={root} />
            {where && <span className="barca-run-summary-where" title={where}>raised at <code>{where.replace(/^.*\/([^/]+\/[^/]+:\d)/, '…/$1')}</code></span>}
            <TraceLink step={step} datadogUrl={datadogUrl} />
          </li>
        })}
        {failed.length > shown.length && <li className="barca-note">and {(failed.length - shown.length).toLocaleString()} more below</li>}
      </ul>
    </>}
    {runError && <div className="barca-run-error" role="alert"><ErrorLine error={runError} large root={root} /></div>}
    <div className="barca-run-actions">
      {!live && <RunActions run={data} failed={runFailed(data.status)} onStarted={onStarted} />}
      <IncidentSummaryButton run={data} datadogUrl={datadogUrl} />
    </div>
  </div>
}

function RunDetails({ data, eventGap, root, datadogUrl, onStarted }: { data: RunView; eventGap: boolean; root?: string | null; datadogUrl: string | null; onStarted: (runId: string) => void }) {
  const [logStep, setLogStep] = useState<string | null>(null)
  const outcome = parseOutcome(data.outcome)
  const envelope = outcome && outcome.status !== 'success' ? outcome.envelope : null
  const result = outcomeResult(outcome)
  const nodes = data.steps_page?.nodes ?? []
  const targets = runTargetLines(data, nodes)
  const summarized = new Set<RunStep>(failedSteps(data).slice(0, SUMMARY_STEPS))
  return <StepToolsContext.Provider value={{ run: data, logStep, setLogStep, onStarted, datadogUrl }}>
    <RunSummary data={data} root={root} datadogUrl={datadogUrl} onStarted={onStarted} onFilter={setLogStep} />
    <Section title="Run">
      {/* The run's facts, two to a line. Its state is said once: by the summary above when a
          step failed or the run did, else here. */}
      <dl className="barca-facts">
        {!summarized.size && !runSummaryError(data) && <Fact label="Status"><StatusBadge size="sm" glyph={runGlyph(data.status)} label={runLabel(data.status)} live={runIsLive(data.status)} /></Fact>}
        <Fact label="Trigger">{triggeredByLabel(data.triggered_by)}</Fact>
        <Fact label="Command"><code>{data.command}</code></Fact>
        <Fact label="Target" wide>{data.target ? <code className="barca-wrap">{data.target}</code> : 'all assets'}</Fact>
        <Fact label="Started"><Absolute at={data.started_at} /></Fact>
        <Fact label="Finished"><Absolute at={data.finished_at} /></Fact>
        <Fact label="Duration">{data.elapsed_seconds === null ? '–' : formatSeconds(data.elapsed_seconds)}</Fact>
        <Fact label="Steps">{runViewSteps(data, nodes)}{data.steps_total !== null && <span className="barca-cell-sub"> · {plural(data.steps_total, 'node')} planned</span>}</Fact>
      </dl>
      {/* What became of the target: unless the one line would repeat the summary's failure. */}
      {!(summarized.size > 0 && targets.length === 1) && targets.map((line) => <p key={line} className="barca-run-target-line">{line}</p>)}
      {/* The error itself is in the summary above, once. */}
      {envelope?.remediation && <p className="barca-note">{envelope.remediation}</p>}
    </Section>
    <Section title="Steps">
      {nodes.length > 0
        ? <div className="barca-run-nodes">{groupRunSteps(data.steps, nodes).map((group) => <RunNodeRow key={group.summary.node_id} group={group} root={root} summarized={summarized} />)}</div>
        : data.steps.length === 0
          ? <p className="barca-note">{runIsLive(data.status) ? 'Waiting for steps…' : 'No step results recorded.'}</p>
          : <StepList steps={data.steps} root={root} summarized={summarized} />}
      {data.steps_page?.truncated && <p className="barca-note">Showing {data.steps.length} of {data.steps_page.total} steps; failed steps are always listed. barca history {data.run_id} --all lists every one.</p>}
    </Section>
    <Section title="Logs">
      {eventGap && runIsLive(data.status) && <p role="status" className="barca-note">Live event replay was incomplete. These logs may show only a retained tail until the run finishes.</p>}
      <LogViewer lines={data.logs.map(log => ({ nodeId: log.node_id, text: log.line, stream: log.stream, ts: log.ts }))} live={runIsLive(data.status)} height={280}
        tools step={logStep} onStepChange={setLogStep} downloadName={`barca-${data.run_id}.log`} />
    </Section>
    {result !== null && result !== undefined && (
      <Section title="Result"><pre className="barca-run-result">{JSON.stringify(result, null, 2)}</pre></Section>
    )}
  </StepToolsContext.Provider>
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
  /** The project root: traceback paths are shown relative to it. */
  root?: string | null
  /** The Datadog web app (`GET /project` `telemetry.datadog_url`): steps link to their traces there. */
  datadogUrl?: string | null
  /** At the end of the toolbar: the page's Run (`PageRunButton`). */
  actions?: ReactNode
  dispatch: Dispatch<RunsMsg>
}

/** The Runs page, for any model, run list and selection: renders and dispatches, nothing else. */
const FILTERS: { value: RunFilter; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'failed', label: 'Not successful' },
  { value: 'live', label: 'In progress' },
]

/**
 * The last day hour by hour, the page's width: a bar per hour, its runs stacked (failures in
 * red at the base, then running, then the rest in a quiet grey), a muted axis every six hours
 * and `now` at the right; a bar filters the list to its hour. Above it, the day in one line.
 */
function ActivityStrip({ activity, hour, now, dispatch }: { activity: RunActivity; hour: number | null; now: number; dispatch: Dispatch<RunsMsg> }) {
  const { zone } = useTimeZone()
  const peak = Math.max(1, ...activity.hours.map((h) => h.ok + h.failed + h.live))
  const width = 100 / activity.hours.length
  const nowAt = 100 - width + (width * ((now % 3_600_000) / 3_600_000))
  return (
    <section className="barca-activity" aria-label="Runs started in the last 24 hours, hour by hour">
      <div className="barca-activity-head">
        <span className="barca-activity-title">Last 24 hours</span>
        <span className="barca-activity-summary">
          {plural(activity.total, 'run')}
          {activity.failed > 0 && <> · <span className="barca-activity-failed">{activity.failed} not successful</span></>}
          {activity.medianSeconds !== null && <> · median {formatSeconds(activity.medianSeconds)}</>}
          {activity.partial && <> · of the runs loaded</>}
        </span>
      </div>
      <div className="barca-activity-chart">
        <div className="barca-activity-bars">
          {activity.hours.map((h) => {
            const n = h.ok + h.failed + h.live
            const label = activityBarLabel(h, zone)
            return (
              <button key={h.at} type="button" className="barca-activity-bar" title={label} aria-label={label} aria-pressed={hour === h.at} disabled={n === 0}
                onClick={() => dispatch({ type: 'hour_toggled', at: h.at })}>
                {n === 0 ? <span className="barca-activity-idle" /> : <span style={{ height: `${Math.max(10, (n / peak) * 100)}%` }}>
                  {h.ok > 0 && <i data-kind="ok" style={{ flexGrow: h.ok }} />}
                  {h.live > 0 && <i data-kind="live" style={{ flexGrow: h.live }} />}
                  {h.failed > 0 && <i data-kind="failed" style={{ flexGrow: h.failed }} />}
                </span>}
              </button>
            )
          })}
        </div>
        <div className="barca-activity-axis" aria-hidden="true">
          {activityTicks(activity.hours, 6, zone).map((t, i) => <span key={t.index} style={{ left: `${t.index * width}%` }}>{t.label}{i === 0 && ` ${zoneName(zone, now)}`}</span>)}
          <span className="barca-activity-now" style={{ left: `${nowAt}%` }}>now</span>
        </div>
      </div>
    </section>
  )
}

export function RunsView({ model, runs, selected, queue, now, root, datadogUrl = null, actions, dispatch }: RunsViewProps) {
  const { zone } = useTimeZone()
  const list = runs.kind === 'ready' ? runs.data : null
  const shown = list ? visibleRuns(list.runs, model) : []
  const failedCount = list ? list.runs.filter((r) => runFailed(r.status)).length : 0
  return (
    <div className="barca-view">
      {/* One toolbar: the title and count, which runs, a search, then Run. */}
      <div className="barca-view-head"><div className="barca-view-bar">
        <div className="barca-view-title">
          <h1>Runs</h1>
          {list && <span className="barca-count">{shown.length === list.runs.length ? list.total.toLocaleString() : `${shown.length.toLocaleString()} of ${list.total.toLocaleString()}`}<span className="barca-sr-only"> {list.total === 1 ? 'run' : 'runs'}</span></span>}
        </div>
        {list && list.runs.length > 0 && <>
          <ChipGroup label="Show" segmented>
            {FILTERS.map((f) => (
              <Chip key={f.value} pressed={model.filter === f.value} onClick={() => dispatch({ type: 'filter_changed', filter: f.value })}>
                {f.label}{f.value === 'failed' && <b>{failedCount}</b>}
              </Chip>
            ))}
          </ChipGroup>
          {model.hour !== null && <Chip pressed onClick={() => dispatch({ type: 'hour_toggled', at: model.hour! })}>{hourLabel(model.hour, zone)}–{hourLabel(model.hour + 3_600_000, zone)} {zoneName(zone, model.hour)} <X size={12} /></Chip>}
        </>}
        <div className="barca-view-actions">
          {list && list.runs.length > 0 && <SearchInput placeholder="Filter runs" label="Filter by target or run id" value={model.query} onChange={(value) => dispatch({ type: 'query_changed', value })} />}
          {actions}
        </div>
      </div></div>
      {queue}
      <div className="barca-assets-split">
        <div className="barca-view-body barca-table-scroll">
          {match(runs)
            .with({ kind: 'loading' }, () => <div aria-label="Loading runs"><Skeleton height={180} /></div>)
            .with({ kind: 'failed' }, ({ error }) => <RunsError error={error} dispatch={dispatch} />)
            .with({ kind: 'ready' }, ({ data, refreshError }) => <>
              {refreshError && <RunsError error={refreshError} dispatch={dispatch} />}
              {data.runs.length > 0 && <ActivityStrip activity={runActivity(data.runs, now, 24, data.truncated)} hour={model.hour} now={now} dispatch={dispatch} />}
              {data.runs.length === 0
                ? <EmptyNote title="No runs yet">Start one with Run all, from an asset's panel, or with <code>barca get</code> in a terminal: every run appears here with its steps and logs.</EmptyNote>
                : shown.length === 0
                  ? <p className="barca-table-empty">No loaded run matches.</p>
                  : <RunTable runs={shown} selected={selected?.id ?? null} now={now} dispatch={dispatch} />}
              {data.truncated && <div className="barca-run-more">
                <p className="barca-note">Showing the latest {plural(data.runs.length, 'run')}.</p>
                {canLoadMore(model) && <Button size="sm" onClick={() => dispatch({ type: 'load_more' })}>Load more</Button>}
              </div>}
            </>)
            .exhaustive()}
        </div>
        {selected && <RunPanelView key={selected.id} id={selected.id} run={selected.run} eventGap={selected.eventGap} root={root} datadogUrl={datadogUrl} dispatch={dispatch} />}
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
    // The whole row opens the run's panel (its error in full there): the row's height never changes.
    return <tr key={run.id} aria-selected={selected !== null && (selected === run.id || selected === run.handle)}
      data-failing={error ? true : undefined}
      onClick={(event) => { if (!(event.target instanceof Element && event.target.closest('button'))) dispatch({ type: 'run_selected', id: run.id }) }}>
    <td><button className="barca-run-link" onClick={() => dispatch({ type: 'run_selected', id: run.id })}>{run.id}</button>{run.command !== 'get' && <span className="barca-cell-sub"> {run.command}</span>}</td>
    <td>{run.target ? <code className="barca-run-target" title={run.target}>{run.target}</code> : <span className="barca-run-target">all assets</span>}</td>
    <td>{triggeredByLabel(run.triggered_by)}</td>
    <td><StatusBadge size="sm" glyph={runGlyph(run.status)} label={runLabel(run.status)} live={runIsLive(run.status)} /></td>
    <td><Age at={run.started_at} nowMs={now} /></td>
    <td className="num">{run.elapsed_seconds === null ? '–' : formatSeconds(run.elapsed_seconds)}</td>
    <td className="num barca-run-steps-cell" title={progress.title}>{progress.steps}</td>
    <td className="barca-run-failure">{error && <ErrorLine error={error.title} text={runFailureLine(run) ?? error.text} inline />}</td>
  </tr>
  })}</tbody></table>
}
