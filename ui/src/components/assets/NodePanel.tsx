import { actionWords } from '@/lib/causesView'
import { keysHref } from '@/lib/keysPage'
import { useEffect, useMemo, useState } from 'react'
import { plural } from '@/lib/plural'
import { Link } from 'react-router'
import { GitBranch, Play, X } from 'lucide-react'
import { Age, Button, ErrorLine, IconButton, KeyValue, RunningBadge, Section, SidePanel, Skeleton, StatusBadge, StatusGlyph } from '@/components'
import { runGlyph } from '@/lib/runs'
import { useTimeZone } from '@/context/timeZone'
import { startedBy } from '@/lib/running'
import { formatAbsolute, formatEstimate } from '@/lib/time'
import { ArtifactSchema } from './ArtifactSchema'
import { AssetVersions } from './AssetVersions'
import { CopyButton, KeyErrors, NodeKeys, PartitionBar } from './NodeKeys'
import { BlastRadius } from './BlastRadius'
import { IncidentControls } from './IncidentControls'
import { LastCheckLine } from './LastCheckLine'
import { EstimateLine } from './EstimateLine'
import { RefreshPathLine } from './RefreshPathLine'
import { CauseLine } from './CauseLine'
import { usePausedSet } from '@/hooks/usePauses'
import { usePinNotes } from '@/hooks/usePins'
import { usePausedRunGuard } from '@/hooks/usePausedRunGuard'
import { useAssetSchema } from '@/hooks/useAssetSchema'
import { useAssetDetail } from '@/hooks/useAssetDetail'
import { useProject } from '@/hooks/useProject'
import { useRunDetail } from '@/hooks/useRuns'
import { usePartitionKey } from '@/hooks/usePartitionGrid'
import { buildRows, formatSeconds } from '@/lib/assetTable'
import { PINNED_HINT, nodeStateDisplay } from '@/lib/cacheStateDisplay'
import { blockerOf } from '@/lib/healthDisplay'
import { freshnessLabel, nodeKindLabel } from '@/lib/status'
import { signalDisplay, signalLine } from '@/lib/dataSignals'
import { EDITOR_ROOTS_KEY, changedSummary, collapseRuns, dataKey, dataKeyNote, downstreamOf, durationHistogram, editorLink, editorOpens, editorRoots, errorFirstLine, failuresOnlyNote, formatBytes, historyBars, keyFigures, onlyFailures, rowsLabel, raisedAtLabel, runPace, runsRecorded, shortHash, sinceLastSuccess, sourcePath, whatChanged } from '@/lib/nodeDetail'
import { consumersOf, downstreamIds, headline, isFailed, streakLabel } from '@/lib/health'
import { isLive, memberHealth } from '@/lib/graphState'
import { shortName } from '@/lib/graph'
import type { AssetRunEntry, KeyArtifact, ListNode, NodeState, NodeStatus } from '@/lib/types'
import type { NodeRunAction } from '@/hooks/useNodeRun'
import type { RollupRoot } from '@/lib/rollupView'

interface NodePanelProps {
  node: NodeState
  /** The states loaded with it (its upstream cone at least): what blocks it. */
  nodes: NodeState[]
  /** The whole graph (`GET /assets`): its consumers, wherever they are. */
  graph: ListNode[]
  nowMs: number
  onSelect: (id: string) => void
  onOpenGraph: (id: string) => void
  onClose: () => void
  runAction: NodeRunAction
  /** The rollup's root list (`GET /state/rollup`), when the page has it: the node's root in a line and what clears it. */
  rollupRoots?: readonly RollupRoot[]
}

/**
 * Everything barca knows about one node: its cache state and why, the last
 * attempt, run history, lineage and declared metadata. All from existing
 * endpoints — `/state` (passed in) and `/assets/{id}` (fetched here).
 */
export function NodePanel({ node, nodes, graph, nowMs, onSelect, onOpenGraph, onClose, runAction, rollupRoots }: NodePanelProps) {
  // The node's root as the root list words it: a lookup by the id on the row.
  const heldBy = rollupRoots?.find((r) => r.id === node.root?.id) ?? null
  const { zone } = useTimeZone()
  // The subject under the state: kept as the panel moves from node to node.
  const [tab, setTab] = useState<PanelTab>('overview')
  const { data: detail, isLoading, isError } = useAssetDetail(node.id)
  const [row] = buildRows([node], nowMs)
  const last = node.last_materialization
  const stats = detail?.stats
  const bars = stats ? historyBars(stats.recent_runs) : []
  // A partitioned node's last materialization is one key; its last attempt is the whole run.
  const lastRun = node.partitioned ? stats?.recent_runs[0] : undefined
  const lastElapsed = lastRun ? lastRun.elapsed_seconds : last?.elapsed_seconds ?? null
  const histogram = stats ? durationHistogram(stats.recent_runs, lastElapsed) : []
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes])
  const upstream = node.inputs
    .map((id) => byId.get(id))
    .filter((n): n is NodeState => n !== undefined)
  const downstream = downstreamOf(node.id, graph)
  const consumers = useMemo(() => consumersOf(graph), [graph])
  const impact = useMemo(() => downstreamIds(node.id, consumers).size, [node.id, consumers])
  const failed = isFailed(memberHealth(node))
  const failedKeys = node.partitions?.failed ?? 0
  // Why a node that did not fail is not up to date: the failing node upstream that blocks it,
  // as the server names it on the row (`root`); a pinned node is never blocked.
  const blocker = failed ? null : blockerOf(node)
  const blockedBy = blocker ? [blocker] : []
  // The one state it shows: `blocked` when a failed node upstream keeps it from running.
  const pins = usePinNotes()
  // Pinned (`barca pin`): it reads `pinned`, not its cache state; a failure above it does not block it.
  const pinned = failed ? null : pins.get(node.id) ?? null
  const failedRun = stats?.recent_runs.find((r) => r.status !== 'success')?.run_id ?? null
  const schema = useAssetSchema(node, upstream)
  const output = schema.data?.find(n => n.id === node.id)
  const change = whatChanged(node.cache, failed)
  const raised = raisedAtLabel(detail?.failure)
  const since = sinceLastSuccess(detail?.failure, node.cache)
  const failuresOnly = stats ? onlyFailures(stats.recent_runs) : false
  const definition = graph.find((n) => n.id === node.id)
  const root = useProject().data?.root
  // A definition without a file (an older server's) has no source line to show.
  const source = definition?.file ? `${sourcePath(root, definition.file)}:${definition.line}` : null
  // The server's path opens in an editor only on a machine that has it: one of the configured roots.
  const roots = editorRoots(typeof localStorage === 'undefined' ? null : localStorage.getItem(EDITOR_ROOTS_KEY))
  const editor = definition?.file && definition.line && editorOpens(root, roots) ? editorLink(root, definition.file, definition.line) : null
  const artifact = node.cache.artifact ?? last?.artifact ?? null
  // A failure's summary (`.barca-why`) says the last attempt: its error, when, how often. Last
  // attempt then shows only what the summary does not, and is left out when that is nothing.
  const summarized = failed && last !== undefined && last !== null && failedKeys === 0
  const lastMore = !!last && (lastElapsed !== null || !!last.partition || !!last.format || !!artifact)
  // How fast the last run that recorded time went, per batched call (`steps_page.nodes` of the run).
  const pacedRun = node.partitioned ? stats?.recent_runs.find((r) => r.elapsed_seconds !== null)?.run_id ?? null : null
  const paced = useRunDetail(pacedRun)
  const pace = runPace(paced.data?.steps_page.nodes?.find((n) => n.node_id === node.id))

  // A partitioned node's last success is its newest successful key's, possibly in a run where
  // other keys failed: said so wherever it is shown beside a failure.
  const successWord = node.partitioned ? 'last success (any key)' : 'last success'
  const figures = node.partitions ? keyFigures(node.partitions, node.last_run) : []
  // Which output the Data tab inspects; a key whose last attempt failed shows what it has cached.
  const shownKey = dataKey(node)
  const cachedKey = usePartitionKey(node.id, shownKey.kind === 'cached' && tab === 'data' ? shownKey.key : null)
  const cachedOutput = cachedKey.data?.output ? { output: cachedKey.data.output, at: cachedKey.data.attempts.find((a) => a.status === 'success')?.created_at ?? null } : null

  const pausedSet = usePausedSet()
  const runGuard = usePausedRunGuard(node.id, runAction.fire)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <SidePanel
      label={`${node.name} details`}
      title={node.name}
      titleText={node.name}
      badge={<span className="barca-sidepanel-kind">{nodeKindLabel(node)}</span>}
      subtitle={node.id}
      subtitleText={node.id}
      subtitleAction={<CopyButton label="Copy id" text={node.id} />}
      actions={
        <>
          {runAction.writes !== 'hidden' && <Button variant="signal" size="sm" iconLeft={<Play size={12} />}
            disabled={runAction.writes === 'disabled'} loading={runAction.triggering}
            title={runAction.title} onClick={runGuard.onRun}>
            Run
          </Button>}
          <IconButton label="Open in graph" size="sm" onClick={() => onOpenGraph(node.id)}>
            <GitBranch size={14} />
          </IconButton>
          <IconButton label="Close (Esc)" size="sm" onClick={onClose}>
            <X size={14} />
          </IconButton>
        </>
      }
    >
      <div className="barca-node-summary">
        {/* The state as the server says it, its reason verbatim, and the last attempt as its own fact. */}
        <div className="barca-state-line">
          {runAction.liveState ? (
            <StatusBadge glyph={runAction.liveState.glyph} label={runAction.liveState.label} live={isLive(runAction.liveState)} />
          ) : row && (
            <>
              <StatusBadge {...nodeStateDisplay({ ...row, blockedBy: blocker, pinned })} />
              {pinned !== null && <span className="barca-cell-sub">{pinned}</span>}
              {pinned === null && !row.lastFailed && blocker && (
                <span className="barca-cell-sub">
                  by <button type="button" className="barca-link" title={`Open ${blocker}`} onClick={() => onSelect(blocker)}>{shortName(blocker)}</button>
                </span>
              )}
            </>
          )}
        </div>
        {runAction.error && <p className="barca-note" role="alert">Could not start run: {runAction.error.message}</p>}
        {/* A partitioned node leads with its keys: how many, cached, and what its newest run did. */}
        {figures.length > 0 && (
          <div className="barca-stats barca-key-figures" role="group" aria-label="Keys">
            {figures.map((f) => (
              <div key={f.id} data-figure={f.id}>
                <b>{f.value.toLocaleString()}</b>
                <span>{f.label}</span>
              </div>
            ))}
          </div>
        )}
        {/* What the recorded row counts and times say about its data: empty, sharply changed, older than an input. */}
        {(node.signals ?? []).length > 0 && (
          <ul className="barca-signals" aria-label="Data signals">
            {(node.signals ?? []).map((signal, i) => (
              <li key={i}>
                <StatusGlyph glyph={signalDisplay(signal).glyph} size={12} />
                <span>{signalDisplay(signal).prefix}{signalLine(signal)}</span>
              </li>
            ))}
          </ul>
        )}
        {/* Why it is not fresh, in the map's word, and the root that holds it, with a link to it. */}
        <CauseLine node={node.id} cause={node.cause} root={node.root} held={heldBy} onSelect={onSelect} />
        {/* What the newest tick that checked it found; the server decides what that means for its health. */}
        {node.last_check && <LastCheckLine check={node.last_check} nowMs={nowMs} />}
        {/* Which schedule's tick will bring it up to date, or that none will: the server's answer. */}
        <RefreshPathLine path={node.refresh_path} nowMs={nowMs} />
        {/* Since when it is out of date and when it is expected fresh, or only rechecked: the server's estimates. */}
        <EstimateLine staleSince={node.stale_since} expectedFreshAt={node.expected_fresh_at} recheckAt={node.recheck_at} noEstimate={node.no_estimate} nowMs={nowMs} />
        {node.running && (
          <KeyValue label="computing now">
            <RunningBadge running={node.running} nowMs={nowMs} />{' '}
            <span className="barca-cell-sub">started by {startedBy(node.running.triggered_by)}</span>
          </KeyValue>
        )}
        {/* A failure's cache detail repeats its error; the block below says it once. */}
        {pinned !== null ? (
          <p className="barca-note">
            {PINNED_HINT}{' '}
            <Button variant="ghost" size="sm" onClick={() => setTab('incident')}>
              Review pin
            </Button>
          </p>
        ) : !(failed && last) && <p className="barca-note">{sentence(node.cache.detail)}</p>}
        {failedKeys > 0 && (
          <div className="barca-why" role="group" aria-label="Why its keys failed">
            <KeyErrors id={node.id} failed={failedKeys} />
            <p className="barca-note">
              {node.failing_since && <>Failing since <Age at={node.failing_since} nowMs={nowMs} /> · {streakLabel(node.consecutive_failures)}</>}
              {node.last_success_at ? <> · {successWord} <Age at={node.last_success_at} nowMs={nowMs} /></> : <> · never succeeded</>}
              {failedRun && <> · <Link to={`/runs?run=${encodeURIComponent(failedRun)}`}>open the run</Link></>}
            </p>
          </div>
        )}
        {failed && last && failedKeys === 0 && (
          <div className="barca-why" role="group" aria-label="Why it failed">
            <div className="barca-why-head">
              <ErrorLine error={last.error} large root={root} />
              {last.error && <CopyButton label="Copy error" text={last.error} />}
            </div>
            <p className="barca-note">
              Failed <Age at={last.created_at} nowMs={nowMs} />
              {node.failing_since && node.consecutive_failures > 1 && <> · failing since <Age at={node.failing_since} nowMs={nowMs} /> · {streakLabel(node.consecutive_failures)}</>}
              {node.last_success_at ? <> · {successWord} <Age at={node.last_success_at} nowMs={nowMs} /></> : <> · never succeeded</>}
              {failedRun && <> · <Link to={`/runs?run=${encodeURIComponent(failedRun)}`}>open the run</Link></>}
            </p>
            {raised && (
              <KeyValue label="raised at">
                <span className="barca-source-path">
                  <code title={raised.full}>{raised.text}</code>
                  <CopyButton label="Copy raising line" text={raised.full.split(' in ')[0]!} />
                </span>
              </KeyValue>
            )}
            {/* "never succeeded" is already said on the line above. */}
            {since && detail?.failure?.last_success && (
              <KeyValue label="since success">
                <span className="barca-what-changed barca-wrap">{since.text}</span>
                {since.at && <span className="barca-cell-sub"> · last success <Age at={since.at} nowMs={nowMs} /></span>}
              </KeyValue>
            )}
          </div>
        )}
        {blockedBy.length > 0 && (
          <div className="barca-why" role="group" aria-label="What it waits on">
            <div className="barca-why-head">
              <strong>Blocked by a failing node upstream</strong>
            </div>
            <ul className="barca-why-list">
              {blockedBy.slice(0, 8).map((id) => (
                <li key={id}>
                  <button type="button" className="barca-link" onClick={() => onSelect(id)}>{shortName(id)}</button>
                  <span>{(() => {
                    const up = byId.get(id)
                    // A partitioned input fails key by key: say how many, not one key's error.
                    const failedKeys = up?.partitions?.failed ?? 0
                    return failedKeys > 0 && up?.partitions
                      ? `${failedKeys.toLocaleString()} of ${up.partitions.total.toLocaleString()} keys failed`
                      : headline(up?.last_materialization?.error)
                  })()}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {change && <p className="barca-note barca-what-changed">{change}</p>}
        {/* What moved the run hash, in a few words; the list (often many helpers) on request. */}
        {node.cache.changed && node.cache.changed.length > 0 && (
          <KeyValue label="changed">
            {node.cache.changed.length <= 3 ? (
              <span className="barca-wrap">{node.cache.changed.join(', ')}</span>
            ) : (
              <details className="barca-changed">
                <summary>{changedSummary(node.cache.changed)}</summary>
                <ul>{node.cache.changed.map((c) => <li key={c}><code>{c}</code></li>)}</ul>
              </details>
            )}
          </KeyValue>
        )}
        {node.partitions && <PartitionBar partitions={node.partitions} />}
        {definition && definition.line > 0 && source !== null && (
          <KeyValue label="defined at">
            <span className="barca-source-path">
              <code title={editor ? undefined : `Relative to the project root ${root ?? ''}. To open it in VS Code, set localStorage ${EDITOR_ROOTS_KEY} to a path prefix this machine has.`}>{source}</code>
              <CopyButton label="Copy path" text={source} />
              {editor && <a className="barca-link" href={editor}>Open in VS Code</a>}
            </span>
          </KeyValue>
        )}
        {rollupRoots?.filter((root) => root.node === node.id).map((root) => {
          const action = actionWords(root)
          return <KeyValue key={root.id} label="root">
            Holds {plural(root.nodes, 'node')} · {root.fresh_by && <>{plural(root.nodes - root.unbounded, 'node')} expected fresh by {formatEstimate(root.fresh_by, zone, nowMs)} </>}
            {root.unbounded > 0 && <>{root.unbounded} with no estimate </>}
            · {action.word} {action.hint} {action.command && <code>{action.command}</code>}
            {root.action.run_once && <> · run once <code>{root.action.run_once}</code></>}
          </KeyValue>
        })}
        <KeyValue label="downstream">
          {impact === 0 ? 'nothing reads it' : impact === 1 ? '1 node depends on it' : `${impact.toLocaleString()} nodes depend on it`}
        </KeyValue>
      </div>

      {/* Below the state, one subject at a time: the panel never asks for a long scroll. */}
      <div className="barca-panel-tabs" role="tablist" aria-label="Asset details">
        {PANEL_TABS.map((t) => (
          <button key={t.id} type="button" role="tab" aria-selected={tab === t.id} onClick={() => setTab(t.id)}>
            {t.label}{t.id === 'history' && stats && stats.total_runs > 0 ? <span className="barca-count" title={runsRecorded(stats.total_runs)}>{stats.total_runs.toLocaleString()}</span> : null}
          </button>
        ))}
      </div>

      {tab === 'overview' && <>
      {(!summarized || lastMore) && <Section title="Last attempt">
        {last ? (
          <>
            {!summarized && <p className="barca-stat-row">
              <StatusBadge glyph={runGlyph((lastRun ?? last).status)} label={(lastRun ?? last).status} />
              {' · '}<Age at={(lastRun ?? last).created_at} nowMs={nowMs} />
              {lastElapsed !== null && <>{' · '}{formatSeconds(lastElapsed)}</>}
              {lastRun?.keys != null && <> · {plural(lastRun.keys, 'key')}{lastRun.failed_keys ? `, ${lastRun.failed_keys.toLocaleString()} failed` : ''}</>}
              {stats && <> · {runsRecorded(stats.total_runs)}</>}
            </p>}
            {/* The distribution of successful durations, when there are two or more to compare
                (`durations.samples`, known before the history loads, so nothing moves). */}
            {lastElapsed !== null && (node.durations?.samples ?? 0) >= 2 && (isLoading || histogram.length > 0) && (
              <div
                className="barca-hist"
                aria-label="Distribution of successful run durations; the last attempt is highlighted"
              >
                {isLoading ? (
                  <Skeleton height={36} />
                ) : (
                  <>
                    <div className="barca-hist-title">Successful run durations{histogram.some((b) => b.isLast) ? ' · the last attempt highlighted' : ''}</div>
                    <div className="barca-hist-bars">
                      {histogram.map((b, i) => (
                        <span
                          key={i}
                          className={b.isLast ? 'is-last' : undefined}
                          style={{ height: `${b.heightPct}%` }}
                          title={b.label}
                        />
                      ))}
                    </div>
                    <div className="barca-hist-axis">
                      <span>fastest {formatSeconds(histogram[0]?.from ?? 0)}</span>
                      <span>slowest {formatSeconds(histogram[histogram.length - 1]?.to ?? 0)}</span>
                    </div>
                  </>
                )}
              </div>
            )}
            {last.partition && <KeyValue label={lastRun ? 'one key' : 'partition'}>{last.partition}</KeyValue>}
            {last.format && (
              <KeyValue label="output">
                {last.format}
                {last.size_bytes !== null && ` · ${formatBytes(last.size_bytes)}`}
              </KeyValue>
            )}
            {artifact && (
              <KeyValue label="path">
                <span className="barca-source-path">
                  <code title={artifact}>{artifact}</code>
                  <CopyButton label="Copy artifact path" text={artifact} />
                </span>
              </KeyValue>
            )}
            {last.error && !failed && <pre className="barca-error">{last.error}</pre>}
          </>
        ) : (
          <p className="barca-note">Not run yet.</p>
        )}
      </Section>}

      {!isLoading && <>
      {node.partitions && (
        <Section title="Keys">
          <NodeKeys key={node.id} id={node.id} partitions={node.partitions} />
        </Section>
      )}

      <details className="barca-node-technical" key={node.id}>
        <summary>Technical details</summary>
        {node.cache.run_hash && (
          <KeyValue label="cache key">
            <code title={node.cache.run_hash}>{shortHash(node.cache.run_hash)}</code>
          </KeyValue>
        )}
        {node.cache.artifact && (
          <KeyValue label="cached result">
            <code className="barca-wrap">{node.cache.artifact}</code>
          </KeyValue>
        )}
        <KeyValue label="runs when">
          {detail ? freshnessLabel(detail.asset) : <Skeleton width={72} height={12} />}
        </KeyValue>
        {row?.nextRunMs != null && (
          <KeyValue label="next run"><Age at={row.nextRunMs} nowMs={nowMs} /></KeyValue>
        )}
        {/* Blast radius lists everything downstream; the direct readers are a detail. */}
        {downstream.length > 0 && (
          <KeyValue label="read directly by">
            {downstream.map((n) => (
              <button key={n.id} type="button" className="barca-link" onClick={() => onSelect(n.id)}>
                {shortName(n.id)}
              </button>
            ))}
          </KeyValue>
        )}
        <KeyValue label="partitioned">{node.partitioned ? 'yes' : 'no'}</KeyValue>
        <KeyValue label="env vars">{node.env.length ? <code>{node.env.join(', ')}</code> : '–'}</KeyValue>
        <KeyValue label="id">
          <code className="barca-wrap">{node.id}</code>
        </KeyValue>
      </details>
      </>}
      </>}
      {tab === 'data' && <>
      <Section title="Inputs & output" action={
        <Button variant="ghost" size="sm" disabled={schema.isFetching} title="Inspect the latest materialized data again. Input schemas describe their own latest outputs."
          onClick={() => void schema.refetch()}>
          {schema.isFetching ? 'Inspecting…' : 'Refresh'}
        </Button>
      }>
        <div className="barca-schema-content">
          {schema.isLoading ? <Skeleton height={120} /> : schema.isError ? (
            <div className="barca-schema-empty">
              <p>Couldn't load schemas: {schema.error.message}</p>
              <Button size="sm" variant="ghost" onClick={() => void schema.refetch()}>Retry</Button>
            </div>
          ) : (
            <>
              <h4 className="barca-schema-label">Output</h4>
              {node.partitioned && (
                <p className="barca-schema-key">
                  {dataKeyNote(shownKey, shownKey.kind === 'cached' ? (cachedKey.isPending ? null : cachedOutput !== null) : null)}{' '}
                  <Link to={keysHref(node.id)}>Key explorer</Link>
                </p>
              )}
              <div className="barca-schema-output">
                {shownKey.kind === 'cached'
                  ? cachedOutput && output
                    ? <ArtifactSchema node={keyOutput(output, cachedOutput.output, cachedOutput.at, shownKey.key)} />
                    : cachedKey.isPending ? <Skeleton height={60} /> : null
                  : output ? <ArtifactSchema node={output} /> : <p className="barca-schema-empty">Output schema unavailable.</p>}
              </div>
              {/* A source node's inputs are one muted line, not a heading over nothing. */}
              {node.inputs.length === 0 ? <p className="barca-schema-empty">No inputs: a source node.</p> : (
                <h4 className="barca-schema-label">Inputs <span>{node.inputs.length}</span></h4>
              )}
              {node.inputs.map(id => {
                const input = schema.data?.find(n => n.id === id)
                const name = input?.name ?? id.split(':').pop() ?? id
                return (
                  <details className="barca-schema-input" key={id}>
                    <summary>{name}</summary>
                    <div className="barca-schema-input-body">
                      <button type="button" className="barca-link" onClick={() => onSelect(id)}>Open {name} →</button>
                      {input ? <ArtifactSchema node={input} /> : <p className="barca-schema-empty">Input schema unavailable.</p>}
                    </div>
                  </details>
                )
              })}
            </>
          )}
        </div>
      </Section>

      </>}
      {tab === 'history' && <>
      <Section title="History">
        {isLoading && (
          <>
            <div className="barca-stats">
              <Skeleton width={220} height={34} />
            </div>
            <Skeleton height={44} style={{ margin: '12px 0 8px' }} />
          </>
        )}
        {isError && <p className="barca-note">Couldn't load run history.</p>}
        {stats && stats.total_runs === 0 && <p className="barca-note">No runs recorded.</p>}
        {stats && failuresOnly && (
          <p className="barca-note">{failuresOnlyNote(stats.total_runs, node.consecutive_failures, stats.recent_runs.length, node.partitioned)}</p>
        )}
        {stats && failuresOnly && <RunList runs={stats.recent_runs} nowMs={nowMs} said={summarized ? last?.error : null} />}
        {stats && stats.total_runs > 0 && !failuresOnly && (
          <>
            <div className="barca-stats">
              <div>
                <b>{stats.total_runs.toLocaleString()}</b>
                <span>{stats.total_runs === 1 ? 'run recorded' : 'runs recorded'}</span>
              </div>
              {pace && (
                <div>
                  <b>{pace.split(' / ')[0]}</b>
                  <span>/ {pace.split(' / ')[1]}</span>
                </div>
              )}
              {stats.median_elapsed_seconds !== null && (
                <div>
                  <b>{formatSeconds(stats.median_elapsed_seconds)}</b>
                  <span>{node.partitioned ? 'median / run' : 'median'}</span>
                </div>
              )}
              {stats.p95_elapsed_seconds !== null && stats.total_runs > 1 && (
                <div>
                  <b>{formatSeconds(stats.p95_elapsed_seconds)}</b>
                  <span>p95</span>
                </div>
              )}
              {stats.max_elapsed_seconds !== null && stats.total_runs > 1 && (
                <div>
                  <b>{formatSeconds(stats.max_elapsed_seconds)}</b>
                  <span>max</span>
                </div>
              )}
              {node.kind !== 'task' && stats.total_runs > 1 && (
                <div>
                  <b>{Math.round(stats.cache_hit_rate * 100)}%</b>
                  <span>re-ran with unchanged inputs</span>
                </div>
              )}
            </div>
            {/* One bar is no trend: the chart starts at two runs. */}
            {bars.length > 1 && <div className="barca-bars" aria-label="Recent run durations, oldest first">
              {bars.map((b, i) => (
                <span
                  key={i}
                  className={b.failed ? 'is-failed' : undefined}
                  style={{ height: `${b.heightPct}%` }}
                  title={`${formatAbsolute(b.createdAt, zone)} · ${b.label}`}
                />
              ))}
            </div>}
            <RunList runs={stats.recent_runs} nowMs={nowMs} said={summarized ? last?.error : null} />
          </>
        )}
      </Section>

      </>}
      {tab === 'incident' && <>
      <IncidentControls node={node} graph={graph} readOnly={runAction.readOnly} />
      {node.partitioned ? (
        <Section title="Repair data">
          <p className="barca-note">Open a key in the <Link to={keysHref(node.id)}>key explorer</Link> to inspect its attempts, upload replacement data, or pin an earlier version.</p>
        </Section>
      ) : <AssetVersions node={node} nowMs={nowMs} readOnly={runAction.readOnly} />}
      <BlastRadius id={node.id} graph={graph} states={byId} paused={pausedSet} onSelect={onSelect} />
      </>}
      {runGuard.dialog}
    </SidePanel>
  )
}

type PanelTab = 'overview' | 'data' | 'history' | 'incident'

/** The panel's subjects under its state, one shown at a time. */
const PANEL_TABS: ReadonlyArray<{ id: PanelTab; label: string }> = [
  { id: 'overview', label: 'Overview' },
  { id: 'data', label: 'Data' },
  { id: 'history', label: 'History' },
  { id: 'incident', label: 'Incident' },
]

/**
 * Recent runs, one line each, a stretch of identical failures folded into one (`collapseRuns`:
 * `×12 · 1h ago – 1m ago`). `said` is the error the panel's summary already shows in full: a run
 * that failed with it says nothing more, so the error is read once.
 */
function RunList({ runs, nowMs, said }: { runs: AssetRunEntry[]; nowMs: number; said?: string | null }) {
  const saidLine = said ? errorFirstLine(said) : null
  return (
    <ul className="barca-runs">
      {collapseRuns(runs).map(({ run: r, count, oldestAt }, i) => (
        <li key={i}>
          <div className="barca-run-line">
            <StatusBadge glyph={runGlyph(r.status)} label={r.status} size="sm" />
            {count > 1 ? (
              <span title={`${count} runs in a row failed the same way`}>×{count} · <Age at={oldestAt} nowMs={nowMs} /> – <Age at={r.created_at} nowMs={nowMs} /></span>
            ) : <Age at={r.created_at} nowMs={nowMs} />}
            {r.elapsed_seconds !== null && count === 1 && <span>{formatSeconds(r.elapsed_seconds)}</span>}
            {r.keys != null && <span>{r.keys.toLocaleString()} keys{r.failed_keys ? `, ${r.failed_keys.toLocaleString()} failed` : ''}{r.empty_keys ? `, ${r.empty_keys.toLocaleString()} empty` : ''}{count > 1 ? ' each' : ''}</span>}
            {r.status === 'success' && rowsLabel(r.rows) && <span title="Rows the run wrote">{rowsLabel(r.rows)}</span>}
            {r.attempts > 1 && <span>{r.attempts} attempts</span>}
          </div>
          {r.error_message && !(saidLine && (errorFirstLine(r.error_message) === saidLine || saidLine.endsWith(errorFirstLine(r.error_message)))) && (
            <details>
              <summary>{errorFirstLine(r.error_message)}</summary>
              <pre className="barca-error">{r.error_message}</pre>
            </details>
          )}
        </li>
      ))}
    </ul>
  )
}

/**
 * The node's output as the schema view reads it, with a key's cached result (`GET
 * /assets/{id}/partitions/key`) in place of its failed last attempt.
 */
function keyOutput(node: NodeStatus, output: KeyArtifact, at: string | null, key: string): NodeStatus {
  return {
    ...node,
    shape: output.shape,
    last_materialization: {
      status: 'success',
      created_at: at ?? '',
      elapsed_seconds: null,
      run_hash: null,
      artifact: output.artifact_path,
      format: output.format,
      size_bytes: output.size_bytes, rows: null,
      partition: key,
    },
  }
}

/** The server's state detail is a clause ("a successful materialization matches…"); shown alone it
 *  reads as a sentence. */
function sentence(text: string): string {
  return text ? text[0]!.toUpperCase() + text.slice(1) : text
}
