import { Fragment, useEffect, useMemo } from 'react'
import { Link } from 'react-router'
import { CircleAlert, Copy, GitBranch, Play, X } from 'lucide-react'
import { Absolute, Age, Button, ErrorLine, IconButton, KeyValue, RunningBadge, Section, SidePanel, Skeleton, StatusBadge, Tag } from '@/components'
import { useTimeZone } from '@/context/timeZone'
import { startedBy } from '@/lib/running'
import { formatAbsolute } from '@/lib/time'
import { ArtifactSchema } from './ArtifactSchema'
import { NodeKeys } from './NodeKeys'
import { useAssetSchema } from '@/hooks/useAssetSchema'
import { useAssetDetail } from '@/hooks/useAssetDetail'
import { useProject } from '@/hooks/useProject'
import { buildRows, formatSeconds, severityStatus } from '@/lib/assetTable'
import { freshnessLabel, nodeKindLabel } from '@/lib/status'
import { downstreamOf, durationHistogram, editorLink, formatBytes, historyBars, shortHash, whatChanged } from '@/lib/nodeDetail'
import { consumersOf, downstreamIds, failedUpstream, headline } from '@/lib/health'
import { memberHealth } from '@/lib/groups'
import { shortName } from '@/lib/graph'
import type { ListNode, NodeState } from '@/lib/types'
import type { NodeRunAction } from '@/hooks/useNodeRun'

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
}

/**
 * Everything barca knows about one node: its cache state and why, the last
 * attempt, run history, lineage and declared metadata. All from existing
 * endpoints — `/state` (passed in) and `/assets/{id}` (fetched here).
 */
/** `a__b__c` with a line-break opportunity after each `__`, so a long name wraps at its parts. */
function Breakable({ name }: { name: string }) {
  const parts = name.split('__')
  return <>{parts.map((part, i) => <Fragment key={i}>{part}{i < parts.length - 1 && <>__<wbr /></>}</Fragment>)}</>
}

function CopyButton({ label, text }: { label: string; text: string }) {
  return <IconButton label={label} size="sm" onClick={() => void navigator.clipboard?.writeText(text)}><Copy size={12} /></IconButton>
}

export function NodePanel({ node, nodes, graph, nowMs, onSelect, onOpenGraph, onClose, runAction }: NodePanelProps) {
  const { zone } = useTimeZone()
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
  const inputs = useMemo(() => new Map(graph.map((n) => [n.id, n.inputs])), [graph])
  const failed = memberHealth(node).status === 'failed'
  const failedKeys = node.partitions?.failed ?? 0
  // Why a node that did not fail is not up to date: something it reads failed.
  const blockedBy = useMemo(() => (failed ? [] : failedUpstream(node.id, inputs, byId)), [failed, node.id, inputs, byId])
  const failedRun = stats?.recent_runs.find((r) => r.status !== 'success')?.run_id ?? null
  const schema = useAssetSchema(node, upstream)
  const output = schema.data?.find(n => n.id === node.id)
  const change = whatChanged(node.cache, failed)
  const definition = graph.find((n) => n.id === node.id)
  const root = useProject().data?.root
  const editor = definition?.line ? editorLink(root, definition.file, definition.line) : null
  const artifact = node.cache.artifact ?? last?.artifact ?? null

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <SidePanel
      label={`${node.name} details`}
      title={<Breakable name={node.name} />}
      badge={<Tag size="sm">{nodeKindLabel(node)}</Tag>}
      subtitle={<span className="barca-wrap">{node.id}</span>}
      actions={
        <>
          <Button variant="signal" size="sm" iconLeft={<Play size={12} />}
            disabled={runAction.readOnly} loading={runAction.triggering}
            title={runAction.title} onClick={runAction.fire}>
            Run
          </Button>
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
        {runAction.liveState ? (
          <StatusBadge status={runAction.liveState.status} label={runAction.liveState.label} />
        ) : row && <StatusBadge status={severityStatus(row.severity)} label={row.stateLabel} />}
        {runAction.error && <p className="barca-note" role="alert">Could not start run: {runAction.error.message}</p>}
        {node.running && (
          <KeyValue label="now">
            <RunningBadge running={node.running} nowMs={nowMs} />{' '}
            <span className="barca-cell-sub">started by {startedBy(node.running.triggered_by)}</span>
          </KeyValue>
        )}
        {/* A failure's cache detail repeats its error; the block below says it once. */}
        {!(failed && last) && <p className="barca-note">{node.cache.detail}</p>}
        {failedKeys > 0 && (
          <p className="barca-note">
            {failedRun && <Link to={`/runs?run=${encodeURIComponent(failedRun)}`}>Open the run</Link>}
            {node.last_success_at && <> · last success <Age at={node.last_success_at} nowMs={nowMs} /></>}
          </p>
        )}
        {failed && last && failedKeys === 0 && (
          <div className="barca-why" role="group" aria-label="Why it failed">
            <div className="barca-why-head">
              <CircleAlert size={14} />
              <ErrorLine error={last.error} large />
            </div>
            <p className="barca-note">
              Failed <Age at={last.created_at} nowMs={nowMs} />
              {node.failing_since && node.consecutive_failures > 1 && <> · failing since <Age at={node.failing_since} nowMs={nowMs} /> · {node.consecutive_failures} runs</>}
              {node.last_success_at ? <> · last success <Age at={node.last_success_at} nowMs={nowMs} /></> : <> · never succeeded</>}
              {failedRun && <> · <Link to={`/runs?run=${encodeURIComponent(failedRun)}`}>open the run</Link></>}
            </p>
          </div>
        )}
        {blockedBy.length > 0 && (
          <div className="barca-why" role="group" aria-label="What it waits on">
            <div className="barca-why-head">
              <CircleAlert size={14} />
              <strong>Waiting on {blockedBy.length} failed upstream node{blockedBy.length === 1 ? '' : 's'}</strong>
            </div>
            <ul className="barca-why-list">
              {blockedBy.slice(0, 8).map((id) => (
                <li key={id}>
                  <button type="button" className="barca-link" onClick={() => onSelect(id)}>{shortName(id)}</button>
                  <span>{headline(byId.get(id)?.last_materialization?.error)}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {change && <p className="barca-note barca-what-changed">{change}</p>}
        {node.cache.changed && node.cache.changed.length > 0 && (
          <KeyValue label="changed">{node.cache.changed.join(', ')}</KeyValue>
        )}
        {node.partitions && <NodeKeys key={node.id} id={node.id} partitions={node.partitions} nowMs={nowMs} />}
        {definition && definition.line > 0 && (
          <KeyValue label="source">
            {editor ? <a className="barca-link" href={editor} title="Open in VS Code">{definition.file}:{definition.line}</a> : <code>{definition.file}:{definition.line}</code>}
            {' '}<CopyButton label="Copy source path" text={`${definition.file}:${definition.line}`} />
          </KeyValue>
        )}
        <KeyValue label="downstream">
          {impact === 0 ? 'nothing reads it' : impact === 1 ? '1 node depends on it' : `${impact.toLocaleString()} nodes depend on it`}
        </KeyValue>
      </div>

      <Section title="Last attempt">
        {last ? (
          <>
            <p className="barca-stat-row">
              <span className={(lastRun ?? last).status === 'failed' ? 'barca-cell-failed' : undefined}>{(lastRun ?? last).status}</span>
              {' · '}<Age at={(lastRun ?? last).created_at} nowMs={nowMs} />
              {' · '}{lastElapsed !== null ? formatSeconds(lastElapsed) : '–'}
              {lastRun?.keys != null && <> · {lastRun.keys.toLocaleString()} keys{lastRun.failed_keys ? `, ${lastRun.failed_keys.toLocaleString()} failed` : ''}</>}
              {stats && <> · {stats.total_runs.toLocaleString()} {stats.total_runs === 1 ? 'run' : 'runs'}</>}
            </p>
            {lastElapsed !== null && (
              <div
                className="barca-hist"
                aria-label="Distribution of successful run durations; the last attempt is highlighted"
              >
                {isLoading ? (
                  <Skeleton height={36} />
                ) : histogram.length > 0 ? (
                  <>
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
                      <span>{formatSeconds(histogram[0]?.from ?? 0)}</span>
                      <span>{formatSeconds(histogram[histogram.length - 1]?.to ?? 0)}</span>
                    </div>
                  </>
                ) : (
                  <p className="barca-hist-note">needs two or more successful runs</p>
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
                <code className="barca-wrap">{artifact}</code> <CopyButton label="Copy artifact path" text={artifact} />
              </KeyValue>
            )}
            {last.error && !failed && <pre className="barca-error">{last.error}</pre>}
          </>
        ) : (
          <p className="barca-note">Not run yet.</p>
        )}
      </Section>

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
        {stats && stats.total_runs > 0 && (
          <>
            <div className="barca-stats">
              <div>
                <b>{stats.total_runs}</b>
                <span>runs</span>
              </div>
              {stats.median_elapsed_seconds !== null && (
                <div>
                  <b>{formatSeconds(stats.median_elapsed_seconds)}</b>
                  <span>median</span>
                </div>
              )}
              {stats.p95_elapsed_seconds !== null && (
                <div>
                  <b>{formatSeconds(stats.p95_elapsed_seconds)}</b>
                  <span>p95</span>
                </div>
              )}
              {stats.max_elapsed_seconds !== null && (
                <div>
                  <b>{formatSeconds(stats.max_elapsed_seconds)}</b>
                  <span>max</span>
                </div>
              )}
              {node.kind !== 'task' && (
                <div>
                  <b>{Math.round(stats.cache_hit_rate * 100)}%</b>
                  <span>re-ran with unchanged inputs</span>
                </div>
              )}
            </div>
            <div className="barca-bars" aria-label="Recent run durations, oldest first">
              {bars.map((b, i) => (
                <span
                  key={i}
                  className={b.failed ? 'is-failed' : undefined}
                  style={{ height: `${b.heightPct}%` }}
                  title={`${formatAbsolute(b.createdAt, zone)} · ${b.label}`}
                />
              ))}
            </div>
            <ul className="barca-runs">
              {stats.recent_runs.map((r, i) => (
                <li key={i}>
                  <div className="barca-run-line">
                    <span className={r.status === 'failed' ? 'barca-cell-failed' : undefined}>
                      {r.status}
                    </span>
                    <Age at={r.created_at} nowMs={nowMs} />
                    <span>{r.elapsed_seconds !== null ? formatSeconds(r.elapsed_seconds) : '–'}</span>
                    {r.keys != null && <span>{r.keys.toLocaleString()} keys{r.failed_keys ? `, ${r.failed_keys.toLocaleString()} failed` : ''}</span>}
                    {r.attempts > 1 && <span>{r.attempts} attempts</span>}
                  </div>
                  {r.error_message && (
                    <details>
                      <summary>{r.error_message.split('\n')[0]}</summary>
                      <pre className="barca-error">{r.error_message}</pre>
                    </details>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
      </Section>

      <Section title="Inputs & output">
        <p className="barca-schema-context">Latest materialized data. Input schemas describe their own latest outputs.</p>
        <Button variant="ghost" size="sm" disabled={schema.isFetching} onClick={() => void schema.refetch()}>
          {schema.isFetching ? 'Inspecting…' : 'Refresh schema'}
        </Button>
        <div className="barca-schema-content">
          {schema.isLoading ? <Skeleton height={360} /> : schema.isError ? (
            <div className="barca-schema-empty">
              <p>Couldn't load schemas: {schema.error.message}</p>
              <Button size="sm" variant="ghost" onClick={() => void schema.refetch()}>Retry</Button>
            </div>
          ) : (
            <>
              <h4 className="barca-schema-label">Output <span>{node.name}</span></h4>
              <div className="barca-schema-output">
                {output ? <ArtifactSchema node={output} /> : <p className="barca-schema-empty">Output schema unavailable.</p>}
              </div>
              <h4 className="barca-schema-label">Inputs <span>{node.inputs.length}</span></h4>
              {node.inputs.length === 0 && <p className="barca-schema-empty">No upstream assets. This is a source node.</p>}
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

      <Section title="Consumers">
        <KeyValue label="downstream">
          {downstream.length === 0
            ? '–'
            : downstream.map((n) => (
                <button key={n.id} type="button" className="barca-link" onClick={() => onSelect(n.id)}>
                  {shortName(n.id)}
                </button>
              ))}
        </KeyValue>
      </Section>

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
          <KeyValue label="next run"><Absolute at={row.nextRunMs} /></KeyValue>
        )}
        <KeyValue label="partitioned">{node.partitioned ? 'yes' : 'no'}</KeyValue>
        <KeyValue label="env vars">{node.env.length ? <code>{node.env.join(', ')}</code> : '–'}</KeyValue>
        <KeyValue label="id">
          <code className="barca-wrap">{node.id}</code>
        </KeyValue>
      </details>
    </SidePanel>
  )
}
