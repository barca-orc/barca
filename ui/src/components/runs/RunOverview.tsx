import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import { useGroups } from '@/hooks/useGroups'
import { recovering } from '@/hooks/polling'
import { api } from '@/lib/api'
import { formatSeconds } from '@/lib/assetTable'
import { shortName } from '@/lib/graph'
import { plural } from '@/lib/plural'
import { runHref } from '@/lib/running'
import { ACTIVE_SHOWN, attemptsLabel, cancellationNotice, runOverview, triggerDetail } from '@/lib/runExplain'
import { runIsLive } from '@/lib/runs'
import type { ActiveStep, NodeGroup, RunView } from '@/lib/types'
import { MiddleName } from '../MiddleName'
import { Section } from '../SidePanel'
import { StatusGlyph } from '../StatusGlyph'
import { RunReasons } from './RunReasons'

/** How many groups the overview lists. */
const GROUPS_SHOWN = 5

export interface RunOverviewViewProps {
  run: RunView
  /** The nodes the run is working on now (`active_steps`); empty when not live or not known. */
  active: readonly ActiveStep[]
  groups: readonly NodeGroup[]
  nowMs: number
}

/**
 * The top of a run's panel (issue 925): why the run was not served from cache, who asked for it and
 * why, how its attempts relate to its steps, why it was cancelled, and, while it runs, how far
 * it is, what it is doing now, by group, how long is left and what took longest.
 */
export function RunOverviewView({ run, active, groups, nowMs }: RunOverviewViewProps) {
  const live = runIsLive(run.status)
  const cancel = cancellationNotice(run.cancellation)
  const attempts = attemptsLabel(run.attempts, run.retries)
  const overview = live ? runOverview(run, active, groups, nowMs) : null
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
        <div className="barca-fact"><dt>Requested by</dt><dd>{triggerDetail(run.triggered_by, run.principal)}</dd></div>
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
            {overview.progress.keys && <> · {overview.progress.keys.done.toLocaleString()} of {plural(overview.progress.keys.total, 'key')} of the nodes in flight</>}
            {' '}· {overview.elapsed}{overview.eta && <> · {overview.eta} at the rate so far</>}
          </p>
          <span className="barca-run-reason-label">Executing now</span>
          {overview.running.length === 0
            ? <p className="barca-note barca-muted">{overview.queued > 0 ? `${plural(overview.queued, 'node')} waiting for a worker.` : 'Between steps.'}</p>
            : <ul className="barca-run-active">
                {overview.running.slice(0, ACTIVE_SHOWN).map((node) => (
                  <li key={node.nodeId}>
                    <StatusGlyph glyph="running" size={12} live />
                    <Link to={`/assets?node=${encodeURIComponent(node.nodeId)}`} title={node.nodeId}><MiddleName name={shortName(node.nodeId)} /></Link>
                    {node.keys && <span className="barca-cell-sub">{node.keys}</span>}
                  </li>
                ))}
                {(overview.running.length > ACTIVE_SHOWN || overview.queued > 0) && (
                  <li className="barca-note barca-muted">
                    {overview.running.length > ACTIVE_SHOWN && <>{(overview.running.length - ACTIVE_SHOWN).toLocaleString()} more running</>}
                    {overview.running.length > ACTIVE_SHOWN && overview.queued > 0 && ' · '}
                    {overview.queued > 0 && <>{overview.queued.toLocaleString()} queued</>}
                  </li>
                )}
              </ul>}
          {overview.groups.length > 0 && <>
            <span className="barca-run-reason-label">By group</span>
            <ul className="barca-run-groups">
              {overview.groups.slice(0, GROUPS_SHOWN).map((g) => (
                <li key={g.group}><MiddleName name={g.group} /><span className="barca-cell-sub">{g.running} running{g.queued > 0 && `, ${g.queued} queued`}</span></li>
              ))}
            </ul>
          </>}
          {overview.slowest.length > 0 && <>
            <span className="barca-run-reason-label">Slowest so far</span>
            <ul className="barca-run-groups">
              {overview.slowest.map((s) => (
                <li key={s.nodeId}><MiddleName name={shortName(s.nodeId)} /><span className="barca-cell-sub">{formatSeconds(s.seconds)}{s.keys > 1 && ` over ${plural(s.keys, 'key')}`}</span></li>
              ))}
            </ul>
          </>}
        </div>
      )}
      {/* A run cancelled in the queue decided nothing: the notice above says so. */}
      {run.cancellation?.started !== false && <>
        <span className="barca-run-reason-label">Why these steps ran</span>
        <RunReasons plan={run.plan_reasons} live={live} />
      </>}
    </Section>
  )
}

/** [`RunOverviewView`] with the nodes a live run is working on now and the project's groups. */
export function RunOverview({ run }: { run: RunView }) {
  const live = runIsLive(run.status)
  const inFlight = useQuery({
    queryKey: ['runs', 'in-flight'],
    queryFn: api.runsInFlight,
    enabled: live,
    refetchInterval: recovering(2_000),
    retry: false,
  })
  const groups = useGroups()
  const row = inFlight.data?.runs.find((r) => r.id === run.run_id || r.run_id === run.run_id)
  return (
    <RunOverviewView
      run={run}
      active={live ? row?.active_steps ?? [] : []}
      groups={groups.data ?? []}
      nowMs={inFlight.dataUpdatedAt}
    />
  )
}
