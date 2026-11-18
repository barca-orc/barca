import { Fragment, createContext, useContext, useMemo, useRef, useState, type ReactNode } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { pathBreaks, relativeTraceback } from '@/lib/health'
import { ChevronDown, ChevronRight, X } from 'lucide-react'
import { Link } from 'react-router'
import { match } from 'ts-pattern'
import { Age, Button, Chip, ChipGroup, EmptyNote, ErrorLine, IconButton, LogViewer, MiddleName, SearchInput, Section, SidePanel, Skeleton, StatusBadge, StatusGlyph } from '@/components'
import { RunOverview } from '@/components/runs/RunOverview'
import { IncidentSummaryButton } from '@/components/IncidentSummary'
import { CancelRun, RetryStep, RunActions } from '@/components/RunActions'
import { useTimeZone } from '@/context/timeZone'
import { failedSteps, failureGroups, raisedAt, traceUrl } from '@/lib/runRecovery'
import { keysHref } from '@/lib/keysPage'
import { formatSeconds } from '@/lib/assetTable'
import { zoneName } from '@/lib/time'
import { plural } from '@/lib/plural'
import type { Dispatch } from '@/lib/program'
import type { Remote } from '@/lib/remote'
import { shortName } from '@/lib/graph'
import { outcomeResult, parseOutcome, runGlyph, runIsLive, runLabel, runNodeOutcome, stepGlyph, triggeredByLabel } from '@/lib/runs'
import { statusGlyph } from '@/lib/status'
import { NO_STEPS_OPEN, activityBarLabel, activityTicks, canLoadMore, runFailed, runNodeTime, runStepRows, runsCommand, runsNarrowed, stepName, toggleStepsOpen, windowLabel, type RunFilter, type RunStepRow, type RunStepsOpen, type RunsModel, type RunsMsg } from '@/lib/runsPage'
import type { ActivityHour, RunActivity, RunList, RunNodeSummary, RunStep, RunView } from '@/lib/types'

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

/** A step's own time, then the time it spent writing its sinks, which is not part of it. */
export function StepTimes({ step }: { step: Pick<RunStep, 'elapsed_seconds' | 'sink_seconds'> }) {
  return <>
    {step.elapsed_seconds !== null && <span className="barca-run-node-time">{formatSeconds(step.elapsed_seconds)}</span>}
    {step.sink_seconds !== null && <span className="barca-run-node-time" title="Writing the step's sinks, after its own time">+ {formatSeconds(step.sink_seconds)} sinks</span>}
  </>
}

/** A step's row: its name, status, time and buttons, then its error. */
function StepRow({ step, name, root, summarized }: { step: RunStep; name: string; root?: string | null; summarized: ReadonlySet<RunStep> }) {
  return <div className="barca-run-step">
    <div className="barca-run-node-line">
      <span className="barca-step-name" title={step.node_id}>{name}</span>
      <StatusBadge size="sm" glyph={stepGlyph(step.status)} label={step.status} />
      <StepTimes step={step} />
      <StepButtons stepId={step.node_id} failed={step.status === 'failed'} step={step} />
    </div>
    <StepError step={step} root={root} summarized={summarized} />
  </div>
}

/** One row of the Steps list (`runStepRows`), one arm per kind. */
function StepListRow({ row, root, summarized, toggle }: { row: RunStepRow; root?: string | null; summarized: ReadonlySet<RunStep>; toggle: (which: keyof RunStepsOpen, id: string) => void }) {
  return match(row)
    .with({ kind: 'idle' }, ({ summary }) => {
      const outcome = runNodeOutcome(summary)
      return <div className="barca-run-node barca-run-node-idle" data-status={summary.status}>
        <span className="barca-step-name" title={summary.node_id}>{shortName(summary.node_id)}</span>
        <StatusBadge size="sm" glyph={statusGlyph(outcome.kind)} label={outcome.label} />
        {outcome.detail && <span className="barca-run-node-detail">{outcome.detail}</span>}
      </div>
    })
    .with({ kind: 'single' }, ({ summary, step }) => {
      const outcome = runNodeOutcome(summary)
      const time = runNodeTime(summary, formatSeconds)
      return <div className="barca-run-node barca-run-node-one" data-status={summary.status}>
        <div className="barca-run-node-line">
          <span className="barca-step-name" title={summary.node_id}>{shortName(summary.node_id)}</span>
          <StatusBadge size="sm" glyph={statusGlyph(outcome.kind)} label={outcome.label} />
          {time && <span className="barca-run-node-time" title={time.title}>{time.text}</span>}
          <StepButtons stepId={step.node_id} failed={step.status === 'failed'} step={step} />
        </div>
        <StepError step={step} root={root} summarized={summarized} />
      </div>
    })
    .with({ kind: 'node' }, ({ group: { summary }, open }) => {
      const outcome = runNodeOutcome(summary)
      const time = runNodeTime(summary, formatSeconds)
      return <div className="barca-run-node barca-run-node-group" data-status={summary.status}>
        <div className="barca-run-node-line">
          <button type="button" className="barca-run-node-toggle" aria-expanded={open} title={open ? `Hide ${shortName(summary.node_id)}'s keys` : `List ${shortName(summary.node_id)}'s keys`}
            onClick={() => toggle('nodes', summary.node_id)}>
            {open ? <ChevronDown size={13} aria-hidden /> : <ChevronRight size={13} aria-hidden />}
            <span className="barca-step-name" title={summary.node_id}>{shortName(summary.node_id)}</span>
          </button>
          <StatusBadge size="sm" glyph={statusGlyph(outcome.kind)} label={outcome.label} />
          {time && <span className="barca-run-node-time" title={time.title}>{time.text}</span>}
          <StepButtons stepId={summary.node_id} failed={summary.failed > 0} />
        </div>
      </div>
    })
    .with({ kind: 'key' }, ({ step }) => <div className="barca-run-key"><StepRow step={step} name={step.partition_key ?? stepName(step.node_id)} root={root} summarized={summarized} /></div>)
    .with({ kind: 'ok_keys' }, ({ node, count, open }) => (
      <div className="barca-run-key">
        <button type="button" className="barca-run-node-toggle barca-run-node-ok" aria-expanded={open} onClick={() => toggle('ok', node)}>
          {open ? <ChevronDown size={12} aria-hidden /> : <ChevronRight size={12} aria-hidden />}
          {count.toLocaleString()} successful key{count === 1 ? '' : 's'}
        </button>
      </div>
    ))
    .with({ kind: 'unlisted' }, ({ count }) => <p className="barca-run-key barca-note">{count.toLocaleString()} more keys not listed here.</p>)
    .with({ kind: 'step' }, ({ step }) => <div className="barca-run-node"><StepRow step={step} name={step.node_id} root={root} summarized={summarized} /></div>)
    .exhaustive()
}

/** A row's height before it is measured, px: one line of a node. */
const STEP_ROW_HEIGHT = 41

/**
 * A run's Steps: a row per planned node (a node's keys are one row with their count, opened
 * to list them), only the rows in view rendered, in a box of its own that scrolls. A failed
 * step's error is said once: in the summary at the top of the panel, or here with its traceback.
 */
function RunSteps({ steps, nodes, root, summarized }: { steps: readonly RunStep[]; nodes: readonly RunNodeSummary[]; root?: string | null; summarized: ReadonlySet<RunStep> }) {
  const [open, setOpen] = useState<RunStepsOpen>(NO_STEPS_OPEN)
  const rows = useMemo(() => runStepRows(steps, nodes, open), [steps, nodes, open])
  const scrollRef = useRef<HTMLDivElement>(null)
  // Not memoized, as the lint notes: its functions are read while rendering, each render.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => STEP_ROW_HEIGHT,
    overscan: 10,
    initialRect: { width: 0, height: STEPS_HEIGHT },
    // Measure a resized line on the next frame: a live log grows while it is measured.
    useAnimationFrameWithResizeObserver: true,
    getItemKey: (index) => stepRowKey(rows[index]!, index),
  })
  const toggle = (which: keyof RunStepsOpen, id: string) => setOpen((o) => toggleStepsOpen(o, which, id))
  return <div ref={scrollRef} className="barca-run-nodes" style={{ maxHeight: STEPS_HEIGHT }}>
    <div className="barca-run-nodes-inner" style={{ height: virtualizer.getTotalSize() }}>
      {virtualizer.getVirtualItems().map((item) => (
        <div key={item.key} ref={virtualizer.measureElement} data-index={item.index} className="barca-run-row" style={{ transform: `translateY(${item.start}px)` }}>
          <StepListRow row={rows[item.index]!} root={root} summarized={summarized} toggle={toggle} />
        </div>
      ))}
    </div>
  </div>
}

/** The Steps box's height at most, px: it scrolls past that. */
const STEPS_HEIGHT = 480

/** A row's identity across renders, so a measured height stays with its row when one opens. */
function stepRowKey(row: RunStepRow, index: number): string {
  return match(row)
    .with({ kind: 'idle' }, ({ summary }) => `n:${summary.node_id}`)
    .with({ kind: 'single' }, ({ summary }) => `n:${summary.node_id}`)
    .with({ kind: 'node' }, ({ group }) => `n:${group.summary.node_id}`)
    .with({ kind: 'key' }, ({ step }) => `k:${step.node_id}:${index}`)
    .with({ kind: 'ok_keys' }, ({ node }) => `ok:${node}`)
    .with({ kind: 'unlisted' }, ({ node }) => `u:${node}`)
    .with({ kind: 'step' }, ({ step }) => `s:${step.node_id}:${index}`)
    .exhaustive()
}

export function RunPanelView({ id, run, eventGap = false, root, datadogUrl = null, dispatch }: { id: string; run: Remote<RunView>; eventGap?: boolean; root?: string | null; datadogUrl?: string | null; dispatch: Dispatch<RunsMsg> }) {
  const retry = <Button size="sm" onClick={() => dispatch({ type: 'retry_detail' })}>Retry</Button>
  return (
    <SidePanel label="Run details" title="Run details" subtitleMono subtitle={run.kind === 'ready' ? run.data.run_id : id} subtitleText={run.kind === 'ready' ? run.data.run_id : id}
      actions={<>
        {run.kind === 'ready' && runIsLive(run.data.status) && <CancelRun key={run.data.run_id} id={run.data.run_id} queued={run.data.status !== 'running'} />}
        <IconButton label="Close run details" size="sm" onClick={() => dispatch({ type: 'panel_closed' })}><X size={14} /></IconButton>
      </>}>
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
  // The keys of a node that failed the same way are one entry: 83 keys, one error.
  const groups = failureGroups(failed)
  const shown = groups.slice(0, SUMMARY_STEPS)
  const hidden = groups.slice(SUMMARY_STEPS).reduce((n, g) => n + Math.max(1, g.keys.length), 0)
  // A run that failed before any step did (it could not plan, say): its own error, said here once.
  const runError = runSummaryError(data)
  return <div className="barca-run-summary">
    {failed.length > 0 && <>
      <strong className="barca-run-summary-head"><StatusGlyph glyph="failed" />{failed.length.toLocaleString()} failed step{failed.length === 1 ? '' : 's'}</strong>
      <ul className="barca-run-summary-failed">
        {shown.map(({ node, first: step, keys }) => {
          const where = raisedAt(step.error_traceback ?? step.error)
          return <li key={`${node}:${step.node_id}`}>
            {keys.length > 1 ? (
              <span className="barca-run-summary-keys">
                <button type="button" className="barca-run-link barca-step-name" title={`Show ${shortName(node)}'s logs`} onClick={() => onFilter(step.node_id)}>{shortName(node)}</button>
                <span className="barca-cell-sub">{keys.length.toLocaleString()} keys · </span>
                <Link className="barca-cell-sub" to={keysHref(node, { filter: { states: ['failed'] } })}>open in the key explorer</Link>
              </span>
            ) : (
              <button type="button" className="barca-run-link barca-step-name" title={`Show ${step.node_id}'s logs`} onClick={() => onFilter(step.node_id)}>{shortName(step.node_id)}</button>
            )}
            <ErrorLine error={step.error_traceback ?? step.error} text={step.error_type && step.error_message ? `${step.error_type}: ${step.error_message}` : undefined} root={root} />
            {where && <span className="barca-run-summary-where" title={where}>raised at <code>{where.replace(/^.*\/([^/]+\/[^/]+:\d)/, '…/$1')}</code></span>}
            <TraceLink step={step} datadogUrl={datadogUrl} />
          </li>
        })}
        {hidden > 0 && <li className="barca-note">and {hidden.toLocaleString()} more below</li>}
      </ul>
    </>}
    {runError && <div className="barca-run-error" role="alert"><ErrorLine error={runError} large root={root} /></div>}
    <div className="barca-run-actions">
      {!live && <RunActions run={data} failed={runFailed(data.status)} onStarted={onStarted} />}
      <IncidentSummaryButton run={data} datadogUrl={datadogUrl} />
    </div>
  </div>
}

/** How many of a run's targets its panel lists by name. */
const TARGETS_SHOWN = 12

function RunDetails({ data, eventGap, root, datadogUrl, onStarted }: { data: RunView; eventGap: boolean; root?: string | null; datadogUrl: string | null; onStarted: (runId: string) => void }) {
  const [logStep, setLogStep] = useState<string | null>(null)
  const outcome = parseOutcome(data.outcome)
  const envelope = outcome && outcome.status !== 'success' ? outcome.envelope : null
  const result = outcomeResult(outcome)
  const nodes = data.steps_page?.nodes ?? []
  const { targets, target_outcomes: outcomes, steps: stepsLine, slowest } = data.explanation
  const summarized = useMemo(() => new Set<RunStep>(failedSteps(data).slice(0, SUMMARY_STEPS)), [data])
  const logLines = useMemo(() => data.logs.map(log => ({ nodeId: log.node_id, text: log.line, stream: log.stream, ts: log.ts })), [data.logs])
  return <StepToolsContext.Provider value={{ run: data, logStep, setLogStep, onStarted, datadogUrl }}>
    <RunSummary data={data} root={root} datadogUrl={datadogUrl} onStarted={onStarted} onFilter={setLogStep} />
    <RunOverview run={data} />
    <Section title="Run">
      {/* The run's facts, two to a line. Its state is said once: by the summary above when a
          step failed or the run did, else here. */}
      <dl className="barca-facts">
        {!summarized.size && !runSummaryError(data) && <Fact label="Status"><StatusBadge size="sm" glyph={runGlyph(data.status)} label={runLabel(data.status)} live={runIsLive(data.status)} /></Fact>}
        <Fact label="Command"><code>{data.command}</code></Fact>
        <Fact label={targets.length > 1 ? `Targets (${targets.length.toLocaleString()})` : 'Target'} wide>
          {targets.length === 0
            ? 'all assets'
            : <ul className="barca-run-targets">
                {targets.slice(0, TARGETS_SHOWN).map((id) => <li key={id} title={id}>{shortName(id)}</li>)}
                {targets.length > TARGETS_SHOWN && <li className="barca-muted">and {(targets.length - TARGETS_SHOWN).toLocaleString()} more</li>}
              </ul>}
        </Fact>
        <Fact label="Started"><Age at={data.started_at} /></Fact>
        <Fact label="Finished"><Age at={data.finished_at} /></Fact>
        <Fact label="Duration">{data.elapsed_seconds === null ? '–' : formatSeconds(data.elapsed_seconds)}</Fact>
        <Fact label="Steps">{stepsLine}{data.steps_total !== null && <span className="barca-cell-sub"> · {plural(data.steps_total, 'node')} planned</span>}</Fact>
      </dl>
      {/* What became of the target: unless the one line would repeat the summary's failure. */}
      {!(summarized.size > 0 && outcomes.length === 1) && outcomes.map((line) => <p key={line} className="barca-run-target-line">{line}</p>)}
      {/* The error itself is in the summary above, once. */}
      {envelope?.remediation && <p className="barca-note">{envelope.remediation}</p>}
    </Section>
    {/* What took longest, once the run has ended (a live run's is in its overview). */}
    {!runIsLive(data.status) && slowest.length > 0 && <Section title="Slowest steps">
      <ul className="barca-run-groups">
        {slowest.map((s) => (
          <li key={s.node_id}>
            <Link to={`/assets?node=${encodeURIComponent(s.node_id)}`} title={s.node_id}><MiddleName name={shortName(s.node_id)} /></Link>
            <span className="barca-cell-sub">{formatSeconds(s.seconds)}{s.keys > 1 && ` over ${plural(s.keys, 'key')}`}</span>
          </li>
        ))}
      </ul>
    </Section>}
    <Section title="Steps">
      {nodes.length === 0 && data.steps.length === 0
        ? <p className="barca-note">{runIsLive(data.status) ? 'Waiting for steps…' : 'No step results recorded.'}</p>
        : <RunSteps steps={data.steps} nodes={nodes} root={root} summarized={summarized} />}
      {data.steps_page?.truncated && <p className="barca-note">Showing {data.steps.length.toLocaleString()} of {data.steps_page.total.toLocaleString()} steps; failed steps are always listed. barca history {data.run_id} --all lists every one.</p>}
    </Section>
    <Section title="Logs">
      {eventGap && runIsLive(data.status) && <p role="status" className="barca-note">Live event replay was incomplete. These logs may show only a retained tail until the run finishes.</p>}
      <LogViewer lines={logLines} live={runIsLive(data.status)} height={280}
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
 * The last day hour by hour, the page's width, as the server counted it over every run of the
 * window the filters keep (`activity` of `GET /runs`): a bar per hour, its runs stacked (the
 * unsuccessful in red at the base, then running, then the rest in a quiet grey), a muted axis
 * every six hours and `now` at the right. A bar's tooltip is the server's hour: its counts, the
 * median time and the leading failure. A bar filters the list to its hour, on the server.
 * Above it, the day in one line, and the zone the hours are shown in.
 */
function ActivityStrip({ activity, window, now, dispatch }: { activity: RunActivity; window: RunsModel['window']; now: number; dispatch: Dispatch<RunsMsg> }) {
  const { zone } = useTimeZone()
  const peak = Math.max(1, ...activity.hours.map((h) => h.runs))
  const width = 100 / activity.hours.length
  const nowAt = 100 - width + (width * ((now % 3_600_000) / 3_600_000))
  const { total } = activity
  return (
    <section className="barca-activity" aria-label="Runs started in the last 24 hours, hour by hour">
      <div className="barca-activity-head">
        <span className="barca-activity-title">Last 24 hours <span className="barca-activity-zone">by hour, {zoneName(zone, now)}</span></span>
        <span className="barca-activity-summary" title={activity.summary}>
          {plural(total.runs, 'run')}
          {total.not_successful > 0 && <> · <span className="barca-activity-failed">{total.not_successful.toLocaleString()} not successful</span></>}
          {total.in_progress > 0 && <> · {total.in_progress.toLocaleString()} in progress</>}
          {total.median_seconds !== null && <> · median {formatSeconds(total.median_seconds)}</>}
        </span>
      </div>
      <div className="barca-activity-chart">
        <div className="barca-activity-bars">
          {activity.hours.map((h) => {
            const label = activityBarLabel(h, zone)
            return (
              <button key={h.at} type="button" className="barca-activity-bar" title={label} aria-label={label} aria-pressed={window?.since === h.at} disabled={h.runs === 0}
                onClick={() => dispatch({ type: 'hour_toggled', window: { since: h.at, until: h.until } })}>
                {h.runs === 0 ? <span className="barca-activity-idle" /> : <span style={{ height: `${Math.max(10, (h.runs / peak) * 100)}%` }}>
                  {h.ok > 0 && <i data-kind="ok" style={{ flexGrow: h.ok }} />}
                  {h.in_progress > 0 && <i data-kind="live" style={{ flexGrow: h.in_progress }} />}
                  {h.not_successful > 0 && <i data-kind="failed" style={{ flexGrow: h.not_successful }} />}
                </span>}
              </button>
            )
          })}
        </div>
        <div className="barca-activity-axis" aria-hidden="true">
          {activityTicks(activity.hours, 6, zone).map((t) => <span key={t.index} style={{ left: `${t.index * width}%` }}>{t.label}</span>)}
          <span className="barca-activity-now" style={{ left: `${nowAt}%` }}>now</span>
        </div>
      </div>
    </section>
  )
}

/**
 * What hiding the schedule's runs left out of the last day (`activity.excluded`, counted by
 * the server): how many, how many of them did not succeed, and the way back to them.
 */
function HiddenSchedule({ hidden, dispatch }: { hidden: ActivityHour; dispatch: Dispatch<RunsMsg> }) {
  return (
    <p className="barca-runs-hidden" role="status">
      <span>{plural(hidden.runs, 'schedule run')} in the last 24 hours {hidden.runs === 1 ? 'is' : 'are'} hidden</span>
      {hidden.not_successful > 0 && <>
        <span aria-hidden> · </span>
        <button type="button" className="barca-run-link barca-activity-failed" title="Show the schedule's runs that did not succeed" onClick={() => dispatch({ type: 'schedule_failures_shown' })}>
          {hidden.not_successful.toLocaleString()} not successful
        </button>
      </>}
      {hidden.in_progress > 0 && <span> · {hidden.in_progress.toLocaleString()} in progress</span>}
      <Button size="sm" variant="ghost" onClick={() => dispatch({ type: 'schedule_toggled' })}>Show schedule runs</Button>
    </p>
  )
}

export function RunsView({ model, runs, selected, queue, now, root, datadogUrl = null, actions, dispatch }: RunsViewProps) {
  const { zone } = useTimeZone()
  const list = runs.kind === 'ready' ? runs.data : null
  const narrowed = runsNarrowed(model)
  // Nothing recorded and nothing asked for: the page has no controls to offer yet.
  const blank = list !== null && list.total === 0 && !narrowed && !list.activity?.excluded?.runs && !model.hideSchedule
  const command = runsCommand(model)
  // The run's panel beside the whole page, toolbar and queue included: full height, whatever the queue holds.
  return (
    <div className="barca-runs">
      <div className="barca-view">
        {/* One toolbar: the title and count, which runs, a search, then Run. */}
        <div className="barca-view-head"><div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>Runs</h1>
            {list && <span className="barca-count" title={`${plural(list.total, 'run')}${narrowed || model.hideSchedule ? ' the filters keep' : ''}: ${command}`}>{list.total.toLocaleString()}<span className="barca-sr-only"> {list.total === 1 ? 'run' : 'runs'}</span></span>}
          </div>
          {list && !blank && <>
            <ChipGroup label="Show" segmented>
              {FILTERS.map((f) => (
                <Chip key={f.value} pressed={model.filter === f.value} onClick={() => dispatch({ type: 'filter_changed', filter: f.value })}>{f.label}</Chip>
              ))}
            </ChipGroup>
            <Chip pressed={model.hideSchedule} title="Leave the runs a schedule started out of the list, its count and the activity (barca history --not-triggered-by schedule); runs that recorded no trigger stay" onClick={() => dispatch({ type: 'schedule_toggled' })}>Hide schedule runs</Chip>
            {model.window !== null && <Chip pressed title="Show every hour" onClick={() => dispatch({ type: 'hour_toggled', window: model.window! })}>{windowLabel(model.window, zone)} <X size={12} /></Chip>}
          </>}
          <div className="barca-view-actions">
            {list && !blank && <SearchInput placeholder="Search runs" label="Search every run by target, job or run id" value={model.query} onChange={(value) => dispatch({ type: 'query_changed', value })} />}
            {actions}
          </div>
        </div></div>
        {queue}
        <div className="barca-view-body barca-table-scroll barca-runs-history">
          {match(runs)
            .with({ kind: 'loading' }, () => <div aria-label="Loading runs"><Skeleton height={180} /></div>)
            .with({ kind: 'failed' }, ({ error }) => <RunsError error={error} dispatch={dispatch} />)
            .with({ kind: 'ready' }, ({ data, refreshError }) => <>
              {refreshError && <RunsError error={refreshError} dispatch={dispatch} />}
              {data.activity && !blank && <ActivityStrip activity={data.activity} window={model.window} now={now} dispatch={dispatch} />}
              {data.activity?.excluded && data.activity.excluded.runs > 0 && <HiddenSchedule hidden={data.activity.excluded} dispatch={dispatch} />}
              {blank
                ? <EmptyNote title="No runs yet">Start one with Run all, from an asset's panel, or with <code>barca get</code> in a terminal: every run appears here with its steps and logs.</EmptyNote>
                : data.runs.length === 0
                  ? <p className="barca-table-empty">
                      {narrowed ? 'No run matches.' : 'No run but the schedule\'s.'}
                      {narrowed && <> <Button size="sm" onClick={() => dispatch({ type: 'filters_cleared' })}>Clear filters</Button></>}
                      {!narrowed && model.hideSchedule && <> <Button size="sm" onClick={() => dispatch({ type: 'schedule_toggled' })}>Show schedule runs</Button></>}
                    </p>
                  : <RunTable runs={data.runs} selected={selected?.id ?? null} now={now} dispatch={dispatch} />}
              {data.truncated && <div className="barca-run-more">
                <p className="barca-note" title={command}>Showing {plural(data.runs.length, 'run')} of {data.total.toLocaleString()}: the ones in progress, then the newest.</p>
                {canLoadMore(model) && <Button size="sm" onClick={() => dispatch({ type: 'load_more' })}>Load more</Button>}
              </div>}
            </>)
            .exhaustive()}
        </div>
      </div>
      {selected && <RunPanelView key={selected.id} id={selected.id} run={selected.run} eventGap={selected.eventGap} root={root} datadogUrl={datadogUrl} dispatch={dispatch} />}
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
    // The row's words are the server's (`line`, as `barca history --json` prints them).
    const failure = run.line.failure
    // The whole row opens the run's panel (its error in full there): the row's height never changes.
    return <tr key={run.id} aria-selected={selected !== null && (selected === run.id || selected === run.handle)}
      data-failing={failure ? true : undefined}
      onClick={(event) => { if (!(event.target instanceof Element && event.target.closest('button'))) dispatch({ type: 'run_selected', id: run.id }) }}>
    <td><button className="barca-run-link" onClick={() => dispatch({ type: 'run_selected', id: run.id })}>{run.id}</button>{run.command !== 'get' && <span className="barca-cell-sub"> {run.command}</span>}</td>
    <td><span className="barca-run-target" title={run.target ?? undefined}>{run.target ?? 'all assets'}</span></td>
    <td>{triggeredByLabel(run.triggered_by)}</td>
    <td><StatusBadge size="sm" glyph={runGlyph(run.status)} label={runLabel(run.status)} live={runIsLive(run.status)} /></td>
    <td><Age at={run.started_at} nowMs={now} /></td>
    <td className="num">{run.elapsed_seconds === null ? '–' : formatSeconds(run.elapsed_seconds)}</td>
    <td className="num barca-run-steps-cell" title={run.line.steps_detail}>{run.line.steps}</td>
    <td className="barca-run-failure">{failure && <ErrorLine error={run.line.failure_detail ?? failure} text={failure} inline />}</td>
  </tr>
  })}</tbody></table>
}
