import { useEffect } from 'react'
import { GitBranch, X } from 'lucide-react'
import { Button, IconButton, KeyValue, Section, SidePanel, Skeleton, StatusBadge, Tag } from '@/components'
import { ArtifactSchema } from './ArtifactSchema'
import { useAssetSchema } from '@/hooks/useAssetSchema'
import { useAssetDetail } from '@/hooks/useAssetDetail'
import { buildRows, formatAgo, formatSeconds, severityStatus } from '@/lib/assetTable'
import { freshnessLabel } from '@/lib/status'
import { downstreamOf, durationHistogram, formatBytes, historyBars, shortHash } from '@/lib/nodeDetail'
import type { NodeState } from '@/lib/types'

interface NodePanelProps {
  node: NodeState
  /** Every node, for lineage. */
  nodes: NodeState[]
  nowMs: number
  onSelect: (id: string) => void
  onOpenGraph: (id: string) => void
  onClose: () => void
}

/**
 * Everything barca knows about one node: its cache state and why, the last
 * attempt, run history, lineage and declared metadata. All from existing
 * endpoints — `/state` (passed in) and `/assets/{id}` (fetched here).
 */
export function NodePanel({ node, nodes, nowMs, onSelect, onOpenGraph, onClose }: NodePanelProps) {
  const { data: detail, isLoading, isError } = useAssetDetail(node.id)
  const [row] = buildRows([node], nowMs)
  const last = node.last_materialization
  const stats = detail?.stats
  const bars = stats ? historyBars(stats.recent_runs) : []
  const histogram = stats ? durationHistogram(stats.recent_runs, last?.elapsed_seconds ?? null) : []
  const upstream = node.inputs
    .map((id) => nodes.find((n) => n.id === id))
    .filter((n): n is NodeState => n !== undefined)
  const downstream = downstreamOf(node.id, nodes)
  const schema = useAssetSchema(node, upstream)
  const output = schema.data?.find(n => n.id === node.id)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <SidePanel
      label={`${node.name} details`}
      title={node.name}
      badge={<Tag size="sm">{node.kind}</Tag>}
      subtitle={row?.file}
      actions={
        <>
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
        {row && <StatusBadge status={severityStatus(row.severity)} label={row.stateLabel} />}
        <p className="barca-note">{node.cache.detail}</p>
        {node.partitions && (
          <>
            <KeyValue label="partitions">
              {node.partitions.cached} of {node.partitions.total} cached
            </KeyValue>
            {node.partitions.missing_keys.length > 0 && (
              <KeyValue label="missing keys">
                <code className="barca-wrap">{node.partitions.missing_keys.join(', ')}</code>
              </KeyValue>
            )}
          </>
        )}
      </div>

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

      <Section title="Last attempt">
        {last ? (
          <>
            <div className="barca-attempt-summary">
              <div>
                <span className="barca-metric-label">Outcome</span>
                <strong className={last.status === 'failed' ? 'barca-cell-failed' : undefined}>{last.status}</strong>
              </div>
              <div>
                <span className="barca-metric-label">Duration</span>
                <strong>{last.elapsed_seconds !== null ? formatSeconds(last.elapsed_seconds) : '–'}</strong>
              </div>
            </div>
            <p className="barca-attempt-time" title={`${last.created_at} UTC`}>{formatAgo(last.created_at, nowMs)} · {last.created_at} UTC</p>
            {last.elapsed_seconds !== null && (
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
            {last.format && (
              <KeyValue label="output">
                {last.format}
                {last.size_bytes !== null && ` · ${formatBytes(last.size_bytes)}`}
              </KeyValue>
            )}
            {last.partition && <KeyValue label="partition">{last.partition}</KeyValue>}
            {last.error && <pre className="barca-error">{last.error}</pre>}
          </>
        ) : (
          <p className="barca-note">Never ran.</p>
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
                  <span>cache hits</span>
                </div>
              )}
            </div>
            <div className="barca-bars" aria-label="Recent run durations, oldest first">
              {bars.map((b, i) => (
                <span
                  key={i}
                  className={b.failed ? 'is-failed' : undefined}
                  style={{ height: `${b.heightPct}%` }}
                  title={`${b.createdAt} UTC · ${b.label}`}
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
                    <span>{formatAgo(r.created_at, nowMs)}</span>
                    <span>{r.elapsed_seconds !== null ? formatSeconds(r.elapsed_seconds) : '–'}</span>
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

      <Section title="Consumers">
        <KeyValue label="downstream">
          {downstream.length === 0
            ? '–'
            : downstream.map((n) => (
                <button key={n.id} type="button" className="barca-link" onClick={() => onSelect(n.id)}>
                  {n.name}
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
          {detail ? freshnessLabel(detail.asset.freshness) : <Skeleton width={72} height={12} />}
        </KeyValue>
        {row?.nextRunMs != null && (
          <KeyValue label="next run">{new Date(row.nextRunMs).toLocaleString()}</KeyValue>
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
