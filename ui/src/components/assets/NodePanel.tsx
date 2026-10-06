import { useEffect } from 'react'
import { GitBranch, X } from 'lucide-react'
import { IconButton, StatusBadge, Tag } from '@/components'
import { useAssetDetail } from '@/hooks/useAssetDetail'
import { buildRows, formatAgo, formatSeconds, severityStatus } from '@/lib/assetTable'
import { freshnessLabel } from '@/lib/status'
import { downstreamOf, formatBytes, historyBars, shortHash } from '@/lib/nodeDetail'
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

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="barca-panel-sect">
      <h3>{title}</h3>
      {children}
    </section>
  )
}

function Row({ k, children }: { k: string; children: React.ReactNode }) {
  return (
    <div className="barca-kv">
      <span>{k}</span>
      <span>{children}</span>
    </div>
  )
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
  const upstream = node.inputs
    .map((id) => nodes.find((n) => n.id === id))
    .filter((n): n is NodeState => n !== undefined)
  const downstream = downstreamOf(node.id, nodes)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <aside className="barca-panel" aria-label={`${node.name} details`}>
      <header className="barca-panel-head">
        <div>
          <div className="barca-panel-title">
            <h2>{node.name}</h2>
            <Tag size="sm">{node.kind}</Tag>
          </div>
          <div className="barca-cell-sub">{row?.file}</div>
        </div>
        <div className="barca-panel-actions">
          <IconButton label="Open in graph" size="sm" onClick={() => onOpenGraph(node.id)}>
            <GitBranch size={14} />
          </IconButton>
          <IconButton label="Close (Esc)" size="sm" onClick={onClose}>
            <X size={14} />
          </IconButton>
        </div>
      </header>

      <Section title="State">
        {row && <StatusBadge status={severityStatus(row.severity)} label={row.stateLabel} />}
        <p className="barca-panel-note">{node.cache.detail}</p>
        {node.cache.run_hash && (
          <Row k="cache key">
            <code title={node.cache.run_hash}>{shortHash(node.cache.run_hash)}</code>
          </Row>
        )}
        {node.cache.artifact && (
          <Row k="cached result">
            <code className="barca-wrap" title={node.cache.artifact}>
              {node.cache.artifact}
            </code>
          </Row>
        )}
        {node.partitions && (
          <>
            <Row k="partitions">
              {node.partitions.cached} of {node.partitions.total} cached
            </Row>
            {node.partitions.missing_keys.length > 0 && (
              <Row k="missing keys">
                <code className="barca-wrap">{node.partitions.missing_keys.join(', ')}</code>
              </Row>
            )}
          </>
        )}
      </Section>

      <Section title="Last attempt">
        {last ? (
          <>
            <Row k="status">
              <span className={last.status === 'failed' ? 'barca-cell-failed' : undefined}>
                {last.status}
              </span>
            </Row>
            <Row k="when">
              {formatAgo(last.created_at, nowMs)} · {last.created_at} UTC
            </Row>
            {last.elapsed_seconds !== null && <Row k="took">{formatSeconds(last.elapsed_seconds)}</Row>}
            {last.format && (
              <Row k="output">
                {last.format}
                {last.size_bytes !== null && ` · ${formatBytes(last.size_bytes)}`}
              </Row>
            )}
            {last.partition && <Row k="partition">{last.partition}</Row>}
            {last.error && <pre className="barca-error">{last.error}</pre>}
          </>
        ) : (
          <p className="barca-panel-note">Never ran.</p>
        )}
      </Section>

      <Section title="History">
        {isLoading && <p className="barca-panel-note">Loading…</p>}
        {isError && <p className="barca-panel-note">Couldn't load run history.</p>}
        {stats && stats.total_runs === 0 && <p className="barca-panel-note">No runs recorded.</p>}
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
              <div>
                <b>{Math.round(stats.cache_hit_rate * 100)}%</b>
                <span>cache hits</span>
              </div>
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

      <Section title="Lineage">
        <Row k="upstream">
          {upstream.length === 0
            ? '–'
            : upstream.map((n) => (
                <button key={n.id} type="button" className="barca-link" onClick={() => onSelect(n.id)}>
                  {n.name}
                </button>
              ))}
        </Row>
        <Row k="downstream">
          {downstream.length === 0
            ? '–'
            : downstream.map((n) => (
                <button key={n.id} type="button" className="barca-link" onClick={() => onSelect(n.id)}>
                  {n.name}
                </button>
              ))}
        </Row>
      </Section>

      <Section title="Metadata">
        {detail && <Row k="runs when">{freshnessLabel(detail.asset.freshness)}</Row>}
        {row?.nextRunMs != null && (
          <Row k="next run">{new Date(row.nextRunMs).toLocaleString()}</Row>
        )}
        <Row k="partitioned">{node.partitioned ? 'yes' : 'no'}</Row>
        <Row k="env vars">{node.env.length ? <code>{node.env.join(', ')}</code> : '–'}</Row>
        <Row k="id">
          <code className="barca-wrap">{node.id}</code>
        </Row>
      </Section>
    </aside>
  )
}
