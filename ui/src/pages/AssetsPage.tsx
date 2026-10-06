import { useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { ArrowDown, ArrowUp, Search } from 'lucide-react'
import { StatusBadge, StatusDot, Tag } from '@/components'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useHealth } from '@/hooks/useHealth'
import { inPipeline, pipelineName } from '@/lib/pipeline'
import {
  DEFAULT_SORT,
  SEVERITIES,
  buildRows,
  filterRows,
  nextSort,
  parseSort,
  severityStatus,
  sortRows,
  summarize,
  type AssetRow,
  type Severity,
  type Sort,
  type SortKey,
} from '@/lib/assetTable'

const SEVERITY_LABEL: Record<Severity, string> = {
  failed: 'failed',
  stale: 'stale',
  never_run: 'never run',
  partial: 'partial',
  unknown: 'unknown',
  always_runs: 'always runs',
  cached: 'cached',
}

function formatNextRun(ms: number | null): string {
  if (ms === null) return 'manual'
  return new Date(ms).toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

/** A column header that sorts the table; the active one shows its direction. */
function SortHeader({
  label,
  column,
  sort,
  onSort,
  numeric = false,
}: {
  label: string
  column: SortKey
  sort: Sort
  onSort: (key: SortKey) => void
  numeric?: boolean
}) {
  const active = sort.key === column
  return (
    <th
      className={numeric ? 'num' : undefined}
      aria-sort={active ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none'}
    >
      <button type="button" className="barca-sort" onClick={() => onSort(column)}>
        {label}
        {active && (sort.dir === 'asc' ? <ArrowUp size={11} /> : <ArrowDown size={11} />)}
      </button>
    </th>
  )
}

export function AssetsPage() {
  const { data, isError, error, dataUpdatedAt } = useAssetStates()
  const { data: health } = useHealth()
  const navigate = useNavigate()
  const [query, setQuery] = useState('')
  const [only, setOnly] = useState<Severity | null>(null)
  // The sort lives in the URL (#/assets?sort=typical&dir=desc), so it survives
  // a reload and can be shared.
  const [params, setParams] = useSearchParams()
  const sort = parseSort(params.get('sort'), params.get('dir'))
  // The pipeline picked in the sidebar (?pipeline=<file>); null = all.
  const pipeline = params.get('pipeline')
  const onSort = (key: SortKey) => {
    const next = nextSort(sort, key)
    const isDefault = next.key === DEFAULT_SORT.key && next.dir === DEFAULT_SORT.dir
    const p = new URLSearchParams(params)
    if (isDefault) {
      p.delete('sort')
      p.delete('dir')
    } else {
      p.set('sort', next.key)
      p.set('dir', next.dir)
    }
    setParams(p, { replace: true })
  }

  // "x ago" is relative to when the data was fetched, so it stays pure and
  // refreshes with every poll.
  const rows = useMemo(
    () => buildRows((data ?? []).filter((n) => inPipeline(n.id, pipeline)), dataUpdatedAt),
    [data, dataUpdatedAt, pipeline],
  )
  const counts = useMemo(() => summarize(rows), [rows])
  const visible = useMemo(() => {
    const searched = filterRows(rows, query)
    const shown = only ? searched.filter((r) => r.severity === only) : searched
    return sortRows(shown, sort)
    // `sort` is rebuilt from the URL each render; its fields are the real deps.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows, query, only, sort.key, sort.dir])

  const open = (row: AssetRow) => navigate(`/graph?focus=${encodeURIComponent(row.id)}`)

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{pipeline ? pipelineName(pipeline) : 'Assets'}</h1>
            {data && (
              <span className="barca-count" title={pipeline ?? undefined}>
                {pipeline ? `${rows.length} of ${data.length} nodes · ${pipeline}` : `${rows.length} nodes`}
              </span>
            )}
          </div>
          <div className="barca-view-actions">
            {health?.read_only && <Tag tone="bare">read-only</Tag>}
            <span className="barca-conn">
              <StatusDot status={health && !isError ? 'success' : 'queued'} size={6} />
              {health ? `barca serve · v${health.version}` : 'offline'}
            </span>
          </div>
        </div>
        <div className="barca-table-tools">
          <label className="barca-search">
            <Search size={13} />
            <input
              placeholder="Search assets and tasks"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              autoFocus
            />
          </label>
          <div className="barca-severity-filter" role="group" aria-label="Filter by state">
            {SEVERITIES.filter((s) => counts[s] > 0).map((s) => (
              <button
                key={s}
                type="button"
                className={only === s ? 'is-on' : undefined}
                aria-pressed={only === s}
                onClick={() => setOnly(only === s ? null : s)}
              >
                <StatusDot status={severityStatus(s)} size={6} />
                {counts[s]} {SEVERITY_LABEL[s]}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div className="barca-view-body">
        {isError ? (
          <p className="barca-table-empty">
            Can't load state: {error instanceof Error ? error.message : 'barca serve is not reachable'}
          </p>
        ) : !data ? (
          <p className="barca-table-empty">Loading…</p>
        ) : visible.length === 0 ? (
          <p className="barca-table-empty">Nothing matches.</p>
        ) : (
          <table className="barca-table">
            <thead>
              <tr>
                <SortHeader label="Name" column="name" sort={sort} onSort={onSort} />
                <SortHeader label="State" column="state" sort={sort} onSort={onSort} />
                <SortHeader label="Last run" column="last" sort={sort} onSort={onSort} />
                <SortHeader label="Typical" column="typical" sort={sort} onSort={onSort} numeric />
                <SortHeader label="Next run" column="next" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody>
              {visible.map((r) => (
                <tr
                  key={r.id}
                  tabIndex={0}
                  onClick={() => open(r)}
                  onKeyDown={(e) => e.key === 'Enter' && open(r)}
                >
                  <td>
                    <div className="barca-cell-name">
                      <span className="name">{r.name}</span>
                      {r.kind !== 'asset' && <Tag size="sm">{r.kind}</Tag>}
                    </div>
                    <div className="barca-cell-sub">{r.file}</div>
                  </td>
                  <td title={r.stateHint}>
                    <StatusBadge status={severityStatus(r.severity)} label={r.stateLabel} size="sm" />
                  </td>
                  <td>
                    {r.last ? (
                      <>
                        <div className={r.last.status === 'failed' ? 'barca-cell-failed' : undefined}>
                          {r.last.status} · {r.last.ago}
                        </div>
                        {r.last.error && (
                          <div className="barca-cell-error" title={r.last.error}>
                            {r.last.error}
                          </div>
                        )}
                      </>
                    ) : (
                      <span className="barca-cell-sub">never</span>
                    )}
                  </td>
                  <td className="num" title={r.p95 ? `p95 ${r.p95}` : undefined}>
                    {r.typical ?? '–'}
                  </td>
                  <td>{formatNextRun(r.nextRunMs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}
