import type { ReactNode } from 'react'
import { X } from 'lucide-react'
import { match } from 'ts-pattern'
import { Button, ConnectionBadge, IconButton, KeyValue, LogViewer, Section, SidePanel, Skeleton, StatusBadge } from '@/components'
import { formatSeconds } from '@/lib/assetTable'
import type { Connection } from '@/lib/connection'
import type { Dispatch } from '@/lib/program'
import type { Remote } from '@/lib/remote'
import { outcomeResult, parseOutcome, runIsLive, runLabel, runStatus, runTime, triggeredByLabel } from '@/lib/runs'
import { canLoadMore, type RunsModel, type RunsMsg } from '@/lib/runsPage'
import type { RunList, RunView } from '@/lib/types'
import './runs.css'

/** One run's details, for any state of its request. */
export function RunPanelView({ id, run, dispatch }: { id: string; run: Remote<RunView>; dispatch: Dispatch<RunsMsg> }) {
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
          <RunDetails data={data} />
        </>)
        .exhaustive()}
    </SidePanel>
  )
}

function RunDetails({ data }: { data: RunView }) {
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
      {(data.error ?? envelope?.error) && <p className="barca-run-error" role="alert">{data.error ?? envelope?.error}</p>}
      {envelope?.remediation && <p className="barca-note">{envelope.remediation}</p>}
    </Section>
    <Section title="Steps">
      {data.steps.length === 0 ? <p className="barca-note">{runIsLive(data.status) ? 'Waiting for steps…' : 'No step results recorded.'}</p> : (
        <ul className="barca-run-steps">
          {data.steps.map((step, index) => <li key={`${step.node_id}:${index}`}>
            <code>{step.node_id}</code>
            <div><StatusBadge size="sm" status={step.status === 'cached' ? 'skipped' : runStatus(step.status)} label={step.status} />
              {step.elapsed_seconds !== null && <span>{formatSeconds(step.elapsed_seconds)}</span>}</div>
            {step.error && <p className="barca-run-error">{step.error}</p>}
          </li>)}
        </ul>
      )}
      {data.steps_page?.truncated && <p className="barca-note">Showing {data.steps.length} of {data.steps_page.total} steps; failed steps are always listed. barca history {data.run_id} --all lists every one.</p>}
    </Section>
    <Section title="Logs">
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
  connection: Connection
  /** `?run=`, with its details; null when no run is selected. */
  selected: { id: string; run: Remote<RunView> } | null
  /** The queue strip (`QueuePanel`, which loads its own data). */
  queue?: ReactNode
  dispatch: Dispatch<RunsMsg>
}

/** The Runs page, for any model, run list and selection: renders and dispatches, nothing else. */
export function RunsView({ model, runs, connection, selected, queue, dispatch }: RunsViewProps) {
  const list = runs.kind === 'ready' ? runs.data : null
  return (
    <div className="barca-view">
      <div className="barca-view-head"><div className="barca-view-bar">
        <div className="barca-view-title"><h1>Runs</h1>{list && <span className="barca-count">{list.total} runs</span>}</div>
        <div className="barca-view-actions"><ConnectionBadge connection={connection} /></div>
      </div></div>
      {queue}
      <div className="barca-assets-split">
        <div className="barca-view-body barca-table-scroll">
          {match(runs)
            .with({ kind: 'loading' }, () => <div aria-label="Loading runs"><Skeleton height={180} /></div>)
            .with({ kind: 'failed' }, ({ error }) => <RunsError error={error} dispatch={dispatch} />)
            .with({ kind: 'ready' }, ({ data, refreshError }) => <>
              {refreshError && <RunsError error={refreshError} dispatch={dispatch} />}
              {data.runs.length === 0
                ? <p className="barca-table-empty">No runs recorded. Start a run from the graph to see it here.</p>
                : <RunTable list={data} selected={selected?.id ?? null} dispatch={dispatch} />}
              {data.truncated && <div className="barca-run-more">
                <p className="barca-note">Showing the latest {data.runs.length} runs.</p>
                {canLoadMore(model) && <Button size="sm" onClick={() => dispatch({ type: 'load_more' })}>Load more</Button>}
              </div>}
            </>)
            .exhaustive()}
        </div>
        {selected && <RunPanelView key={selected.id} id={selected.id} run={selected.run} dispatch={dispatch} />}
      </div>
    </div>
  )
}

function RunsError({ error, dispatch }: { error: string; dispatch: Dispatch<RunsMsg> }) {
  return <p className="barca-table-empty" role="alert">Can't load runs: {error} <Button size="sm" onClick={() => dispatch({ type: 'retry_list' })}>Retry</Button></p>
}

function RunTable({ list, selected, dispatch }: { list: RunList; selected: string | null; dispatch: Dispatch<RunsMsg> }) {
  return <table className="barca-table barca-run-table"><thead><tr>
    <th>Run</th><th>Target</th><th>Trigger</th><th>Status</th><th>Started</th><th className="num">Duration</th><th className="num">Steps</th>
  </tr></thead><tbody>{list.runs.map(run => <tr key={run.id} aria-selected={selected === run.id || selected === run.handle}>
    <td><button className="barca-run-link" onClick={() => dispatch({ type: 'run_selected', id: run.id })}>{run.id}</button><div className="barca-cell-sub">{run.command}</div></td>
    <td><code className="barca-run-target" title={run.target ?? undefined}>{run.target ?? 'all assets'}</code></td>
    <td>{triggeredByLabel(run.triggered_by)}</td>
    <td><StatusBadge size="sm" status={runStatus(run.status)} label={runLabel(run.status)} /></td>
    <td>{runTime(run.started_at)}</td>
    <td className="num">{run.elapsed_seconds === null ? '–' : formatSeconds(run.elapsed_seconds)}</td>
    <td className="num">{run.steps_executed} ran · {run.steps_cached} cached</td>
  </tr>)}</tbody></table>
}
