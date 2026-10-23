import { useCallback, useEffect, useState } from 'react'
import { useSearchParams } from 'react-router'
import { X } from 'lucide-react'
import { Button, ConnectionBadge, IconButton, KeyValue, LogViewer, Section, SidePanel, Skeleton, StatusBadge } from '@/components'
import { useHealth } from '@/hooks/useHealth'
import { useRunDetail, useRuns } from '@/hooks/useRuns'
import { connection } from '@/lib/connection'
import { formatSeconds } from '@/lib/assetTable'
import { outcomeResult, parseOutcome, runIsLive, runLabel, runStatus, runTime, triggeredByLabel } from '@/lib/runs'
import { QueuePanel } from './QueuePanel'
import './runs.css'

function RunPanel({ id, onClose, onResolved }: { id: string; onClose: () => void; onResolved: (id: string) => void }) {
  const { data, error, isError, refetch } = useRunDetail(id)
  const durableId = data?.run_id
  useEffect(() => {
    // Replace an older link's temporary server handle with the run id, so
    // bookmarked details continue to work after the server restarts.
    if (durableId && durableId !== id) onResolved(durableId)
  }, [durableId, id, onResolved])
  const outcome = parseOutcome(data?.outcome)
  const envelope = outcome && outcome.status !== 'success' ? outcome.envelope : null
  const result = outcomeResult(outcome)
  return (
    <SidePanel label="Run details" title="Run details" subtitle={data?.run_id ?? id}
      actions={<IconButton label="Close run details" size="sm" onClick={onClose}><X size={14} /></IconButton>}>
      {!data ? isError ? (
        <Section title="Run unavailable">
          <p role="alert">{error.message}</p>
          <Button size="sm" onClick={() => void refetch()}>Retry</Button>
        </Section>
      ) : <Skeleton height={180} /> : (
        <>
          {isError && <Section title="Updates unavailable">
            <p role="alert">Showing the last loaded run information: {error.message}</p>
            <Button size="sm" onClick={() => void refetch()}>Retry</Button>
          </Section>}
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
          </Section>
          <Section title="Logs">
            <LogViewer lines={data.logs.map(log => ({ nodeId: log.node_id, text: log.line, stream: log.stream, ts: log.ts }))} live={runIsLive(data.status)} height={280} />
          </Section>
          {result !== null && result !== undefined && (
            <Section title="Result"><pre className="barca-run-result">{JSON.stringify(result, null, 2)}</pre></Section>
          )}
        </>
      )}
    </SidePanel>
  )
}

export function RunsPage() {
  const [params, setParams] = useSearchParams()
  const selected = params.get('run')
  const [limit, setLimit] = useState(100)
  const { data, isError, error, refetch } = useRuns(limit)
  const health = useHealth()
  const resolve = useCallback((id: string) => {
    setParams(previous => {
      const next = new URLSearchParams(previous)
      next.set('run', id)
      return next
    }, { replace: true })
  }, [setParams])
  const select = (id: string | null) => {
    const next = new URLSearchParams(params)
    if (id) next.set('run', id)
    else next.delete('run')
    setParams(next)
  }
  return (
    <div className="barca-view">
      <div className="barca-view-head"><div className="barca-view-bar">
        <div className="barca-view-title"><h1>Runs</h1>{data && <span className="barca-count">{data.total} runs</span>}</div>
        <div className="barca-view-actions"><ConnectionBadge connection={connection(health.data, health.isError)} /></div>
      </div></div>
      <QueuePanel readOnly={health.data?.read_only} />
      <div className="barca-assets-split">
        <div className="barca-view-body barca-table-scroll">
          {isError && <p className="barca-table-empty" role="alert">Can't load runs: {error.message} <Button size="sm" onClick={() => void refetch()}>Retry</Button></p>}
          {!data && !isError && <div aria-label="Loading runs"><Skeleton height={180} /></div>}
          {data && data.runs.length === 0 && <p className="barca-table-empty">No runs recorded. Start a run from the graph to see it here.</p>}
          {data && data.runs.length > 0 && <table className="barca-table barca-run-table"><thead><tr>
            <th>Run</th><th>Target</th><th>Trigger</th><th>Status</th><th>Started</th><th className="num">Duration</th><th className="num">Steps</th>
          </tr></thead><tbody>{data.runs.map(run => <tr key={run.id} aria-selected={selected === run.id || selected === run.handle}>
            <td><button className="barca-run-link" onClick={() => select(run.id)}>{run.id}</button><div className="barca-cell-sub">{run.command}</div></td>
            <td><code className="barca-run-target" title={run.target ?? undefined}>{run.target ?? 'all assets'}</code></td>
            <td>{triggeredByLabel(run.triggered_by)}</td>
            <td><StatusBadge size="sm" status={runStatus(run.status)} label={runLabel(run.status)} /></td>
            <td>{runTime(run.started_at)}</td>
            <td className="num">{run.elapsed_seconds === null ? '–' : formatSeconds(run.elapsed_seconds)}</td>
            <td className="num">{run.steps_executed} ran · {run.steps_cached} cached</td>
          </tr>)}</tbody></table>}
          {data?.truncated && <div className="barca-run-more">
            <p className="barca-note">Showing the latest {data.runs.length} runs.</p>
            {limit < 1000 && <Button size="sm" onClick={() => setLimit(Math.min(limit + 100, 1000))}>Load more</Button>}
          </div>}
        </div>
        {selected && <RunPanel key={selected} id={selected} onResolved={resolve} onClose={() => select(null)} />}
      </div>
    </div>
  )
}
