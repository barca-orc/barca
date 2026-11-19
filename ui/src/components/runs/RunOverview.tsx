import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import { recovering } from '@/hooks/polling'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { plural } from '@/lib/plural'
import { elapsedLabel, runHref } from '@/lib/running'
import { runIsLive } from '@/lib/runs'
import type { RunNow, RunView } from '@/lib/types'
import { MiddleName } from '../MiddleName'
import { Section } from '../SidePanel'
import { StatusGlyph } from '../StatusGlyph'
import { RunReasons } from './RunReasons'
import { formatSeconds } from '@/lib/time'

/** How many nodes the overview lists as executing now. */
const ACTIVE_SHOWN = 8

/** How many groups the overview lists. */
const GROUPS_SHOWN = 5

/** A live run the server has not listed as in flight yet: nothing executing, nothing queued. */
const NOTHING_NOW: RunNow = { running: [], queued: 0, keys: null, groups: [], line: '' }

export interface RunOverviewViewProps {
  run: RunView
  /** What the run is working on now (`now` of `GET /runs?status=running`); null when not live or not known. */
  now: RunNow | null
  nowMs: number
}

/**
 * The top of a run's panel: why the run was not served from cache, who asked for it and why,
 * its retries, why it was cancelled, and, while it runs, how far it is, what it is doing now,
 * by group, how long is left and what took longest. The words are the server's
 * (`run.explanation`), and so are the nodes in flight and their groups (`now`).
 */
export function RunOverviewView({ run, now, nowMs }: RunOverviewViewProps) {
  const live = runIsLive(run.status)
  const { cancellation: cancel, attempts, progress, slowest, why } = run.explanation
  const overview = live && progress ? { progress, now: now ?? NOTHING_NOW, elapsed: elapsedLabel(run.started_at, nowMs) } : null
  return (
    <Section title="Run overview">
      {cancel && (
        <div className="barca-run-cancel" role="status">
          <strong>{cancel.headline}</strong>
          {cancel.detail && <span className="barca-note barca-muted"> {cancel.detail}</span>}
          {cancel.run && <> <Link to={runHref(cancel.run)}>Open run {cancel.run}</Link></>}
        </div>
      )}
      <dl className="barca-facts">
        <div className="barca-fact"><dt>Requested by</dt><dd>{run.explanation.requested_by}</dd></div>
        {attempts && <div className="barca-fact"><dt>Attempts</dt><dd>{attempts}</dd></div>}
        {run.trigger_reason && <div className="barca-fact" data-wide><dt>Reason</dt><dd>“{run.trigger_reason}”</dd></div>}
      </dl>
      {overview && (
        <div className="barca-run-live">
          <div className="barca-run-progress" aria-label={overview.progress.percent === null ? 'progress unknown' : `${overview.progress.percent}% done`}>
            <span style={{ width: `${overview.progress.percent ?? 0}%` }} />
          </div>
          <p className="barca-note">
            {overview.progress.percent !== null && <>{overview.progress.percent}% · </>}
            {overview.progress.done.toLocaleString()}{overview.progress.total !== null && <> of {plural(overview.progress.total, 'step')}</>}
            {' '}({overview.progress.executed.toLocaleString()} executed, {overview.progress.cached.toLocaleString()} cached)
            {overview.now.keys && <> · {overview.now.keys.done.toLocaleString()} of {plural(overview.now.keys.total, 'key')} of the nodes in flight</>}
            {' '}· {overview.elapsed}{overview.progress.eta && <> · {overview.progress.eta} at the rate so far</>}
          </p>
          <span className="barca-run-reason-label">Executing now</span>
          {overview.now.running.length === 0
            ? <p className="barca-note barca-muted">{overview.now.queued > 0 ? `${plural(overview.now.queued, 'node')} waiting.` : 'Between steps.'}</p>
            : <ul className="barca-run-active">
                {overview.now.running.slice(0, ACTIVE_SHOWN).map((node) => (
                  <li key={node.node_id}>
                    <StatusGlyph glyph="running" size={12} live />
                    <Link to={`/assets?node=${encodeURIComponent(node.node_id)}`} title={node.node_id}><MiddleName name={shortName(node.node_id)} /></Link>
                    {node.keys && <span className="barca-cell-sub">{node.keys}</span>}
                  </li>
                ))}
                {(overview.now.running.length > ACTIVE_SHOWN || overview.now.queued > 0) && (
                  <li className="barca-note barca-muted">
                    {overview.now.running.length > ACTIVE_SHOWN && <>{(overview.now.running.length - ACTIVE_SHOWN).toLocaleString()} more running</>}
                    {overview.now.running.length > ACTIVE_SHOWN && overview.now.queued > 0 && ' · '}
                    {overview.now.queued > 0 && <>{overview.now.queued.toLocaleString()} waiting</>}
                  </li>
                )}
              </ul>}
          {overview.now.groups.length > 0 && <>
            <span className="barca-run-reason-label">By group</span>
            <ul className="barca-run-groups">
              {overview.now.groups.slice(0, GROUPS_SHOWN).map((g) => (
                <li key={g.id}><MiddleName name={g.group} /><span className="barca-cell-sub">{g.running} running{g.queued > 0 && `, ${g.queued} waiting`}</span></li>
              ))}
            </ul>
          </>}
          {slowest.length > 0 && <>
            <span className="barca-run-reason-label">Slowest so far</span>
            <ul className="barca-run-groups">
              {slowest.map((s) => (
                <li key={s.node_id}><MiddleName name={shortName(s.node_id)} /><span className="barca-cell-sub">{formatSeconds(s.seconds)}{s.keys > 1 && ` over ${plural(s.keys, 'key')}`}</span></li>
              ))}
            </ul>
          </>}
        </div>
      )}
      {/* A run cancelled in the queue decided nothing: the notice above says so. */}
      {run.cancellation?.started !== false && <>
        <span className="barca-run-reason-label">Why these steps ran</span>
        <RunReasons why={why} />
      </>}
    </Section>
  )
}

/** [`RunOverviewView`] with what a live run is working on now. */
export function RunOverview({ run }: { run: RunView }) {
  const live = runIsLive(run.status)
  const inFlight = useQuery({
    queryKey: ['runs', 'in-flight'],
    queryFn: api.runsInFlight,
    enabled: live,
    refetchInterval: recovering(2_000),
    retry: false,
  })
  const row = inFlight.data?.runs.find((r) => r.id === run.run_id || r.run_id === run.run_id)
  return (
    <RunOverviewView
      run={run}
      now={live ? row?.now ?? null : null}
      nowMs={inFlight.dataUpdatedAt}
    />
  )
}
