import { useMemo } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { ArrowDown, ArrowUp, Search, X } from 'lucide-react'
import { StatusBadge, StatusDot, Tag } from '@/components'
import { useAssetStates } from '@/hooks/useAssetStates'
import { NodePanel } from '@/components/assets/NodePanel'
import { useHealth } from '@/hooks/useHealth'
import { inPipeline, pipelineName } from '@/lib/pipeline'
import {
  EMPTY_FILTERS,
  KINDS,
  LAST_RUNS,
  applyFilters,
  facetCounts,
  isFiltered,
  parseFilters,
  writeFilters,
  type Filters,
  type Kind,
  type LastRun,
} from '@/lib/assetFilters'
import {
  DEFAULT_SORT,
  SEVERITIES,
  buildRows,
  nextSort,
  parseSort,
  severityStatus,
  sortRows,
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

const KIND_LABEL: Record<Kind, string> = { asset: 'assets', task: 'tasks', sensor: 'sensors' }

const LAST_RUN_LABEL: Record<LastRun, string> = {
  any: 'any time',
  hour: 'last hour',
  day: 'last 24h',
  week_plus: '7+ days ago',
  never: 'never',
}

/** Add or remove `v` from a multi-select list. */
function toggle<T>(list: T[], v: T): T[] {
  return list.includes(v) ? list.filter((x) => x !== v) : [...list, v]
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
  // Filters live in the URL with the sort and pipeline, so a filtered view can
  // be reloaded and shared.
  const filters = parseFilters(params)
  const setFilters = (f: Filters) => setParams(writeFilters(params, f), { replace: true })
  const now = dataUpdatedAt
  const counts = facetCounts(rows, filters, now)
  const visible = sortRows(applyFilters(rows, filters, now), sort)

  // The node shown in the side panel (?node=<id>).
  const selectedId = params.get('node')
  const selectedNode = data?.find((n) => n.id === selectedId) ?? null
  const setSelected = (id: string | null) => {
    const p = new URLSearchParams(params)
    if (id === null) p.delete('node')
    else p.set('node', id)
    setParams(p, { replace: true })
  }
  const open = (row: AssetRow) => setSelected(row.id === selectedId ? null : row.id)
  const openGraph = (id: string) => navigate(`/graph?focus=${encodeURIComponent(id)}`)

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
              placeholder="Search by name or file"
              value={filters.query}
              onChange={(e) => setFilters({ ...filters, query: e.target.value })}
              autoFocus
            />
          </label>

          <div className="barca-chips" role="group" aria-label="Kind">
            {KINDS.filter((k) => counts.kinds[k] > 0 || filters.kinds.includes(k)).map((k) => (
              <button
                key={k}
                type="button"
                className={filters.kinds.includes(k) ? 'is-on' : undefined}
                aria-pressed={filters.kinds.includes(k)}
                onClick={() => setFilters({ ...filters, kinds: toggle(filters.kinds, k) })}
              >
                {counts.kinds[k]} {KIND_LABEL[k]}
              </button>
            ))}
          </div>

          <label className="barca-select">
            <span>Last run</span>
            <select
              value={filters.lastRun}
              onChange={(e) => setFilters({ ...filters, lastRun: e.target.value as LastRun })}
            >
              {LAST_RUNS.map((l) => (
                <option key={l} value={l}>
                  {LAST_RUN_LABEL[l]}
                  {l === 'any' ? '' : ` (${counts.lastRun[l]})`}
                </option>
              ))}
            </select>
          </label>

          {(counts.scheduled > 0 || filters.scheduled) && (
            <div className="barca-chips">
              <button
                type="button"
                className={filters.scheduled ? 'is-on' : undefined}
                aria-pressed={filters.scheduled}
                onClick={() => setFilters({ ...filters, scheduled: !filters.scheduled })}
              >
                {counts.scheduled} scheduled
              </button>
            </div>
          )}
          {(counts.partitioned > 0 || filters.partitioned) && (
            <div className="barca-chips">
              <button
                type="button"
                className={filters.partitioned ? 'is-on' : undefined}
                aria-pressed={filters.partitioned}
                onClick={() => setFilters({ ...filters, partitioned: !filters.partitioned })}
              >
                {counts.partitioned} partitioned
              </button>
            </div>
          )}

          {isFiltered(filters) && (
            <button type="button" className="barca-clear" onClick={() => setFilters(EMPTY_FILTERS)}>
              <X size={12} /> Clear filters
            </button>
          )}
        </div>

        <div className="barca-table-tools">
          <div className="barca-chips" role="group" aria-label="State">
            {SEVERITIES.filter((s) => counts.states[s] > 0 || filters.states.includes(s)).map((s) => (
              <button
                key={s}
                type="button"
                className={filters.states.includes(s) ? 'is-on' : undefined}
                aria-pressed={filters.states.includes(s)}
                onClick={() => setFilters({ ...filters, states: toggle(filters.states, s) })}
              >
                <StatusDot status={severityStatus(s)} size={6} />
                {counts.states[s]} {SEVERITY_LABEL[s]}
              </button>
            ))}
          </div>
          <span className="barca-count barca-shown">
            {visible.length === rows.length ? `${rows.length} shown` : `${visible.length} of ${rows.length} shown`}
          </span>
        </div>
      </div>

      <div className="barca-assets-split">
      <div className="barca-view-body barca-table-scroll">
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
                  aria-selected={r.id === selectedId}
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
      {selectedNode && (
        <NodePanel
          node={selectedNode}
          nodes={data ?? []}
          nowMs={dataUpdatedAt}
          onSelect={setSelected}
          onOpenGraph={openGraph}
          onClose={() => setSelected(null)}
        />
      )}
      </div>
    </div>
  )
}
