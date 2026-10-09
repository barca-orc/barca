import { useEffect, useMemo, useRef } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { ArrowDown, ArrowUp, X } from 'lucide-react'
import {
  Chip,
  ChipGroup,
  ConnectionBadge,
  SearchInput,
  Select,
  Skeleton,
  StatusBadge,
  StatusDot,
  Tag,
} from '@/components'
import { useAssetStates } from '@/hooks/useAssetStates'
import { NodePanel } from '@/components/assets/NodePanel'
import { useHealth } from '@/hooks/useHealth'
import { connection } from '@/lib/connection'
import { inPipeline, pipelineName } from '@/lib/pipeline'
import {
  EMPTY_FILTERS,
  LAST_RUNS,
  applyFilters,
  facetCounts,
  isFiltered,
  parseFilters,
  writeFilters,
  type Filters,
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
import type { NodeKind } from '@/lib/types'

const SEVERITY_LABEL: Record<Severity, string> = {
  failed: 'failed',
  stale: 'stale',
  never_run: 'never run',
  partial: 'partial',
  unknown: 'unknown',
  always_runs: 'always runs',
  cached: 'cached',
}

const PAGE_LABEL: Record<NodeKind, string> = { asset: 'Assets', task: 'Tasks', sensor: 'Sensors' }

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

export function AssetsPage({ kind = 'asset' }: { kind?: NodeKind }) {
  const { data, isError, error, dataUpdatedAt } = useAssetStates()
  const { data: health, isError: healthError } = useHealth()
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
    () => buildRows((data ?? []).filter((n) => n.kind === kind && inPipeline(n.id, pipeline)), dataUpdatedAt),
    [data, dataUpdatedAt, pipeline, kind],
  )
  // Filters live in the URL with the sort and pipeline, so a filtered view can
  // be reloaded and shared.
  const filters = { ...parseFilters(params), kinds: [] }
  const setFilters = (f: Filters) => setParams(writeFilters(params, f), { replace: true })
  const now = dataUpdatedAt
  const counts = facetCounts(rows, filters, now)
  const visible = sortRows(applyFilters(rows, filters, now), sort)
  const tableBody = useRef<HTMLTableSectionElement>(null)

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
  const openGraph = (id: string) => {
    const p = new URLSearchParams()
    p.set('view', 'graph')
    p.set('focus', id)
    if (pipeline) p.set('pipeline', pipeline)
    navigate(`/assets?${p}`)
  }

  useEffect(() => {
    if (!selectedId) return
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
      if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
      // Keep text editing and native controls' arrow-key behavior intact.
      if (event.target instanceof Element && event.target.closest(
        'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"], [role="combobox"], [role="slider"], [role="spinbutton"]',
      )) return
      const index = visible.findIndex((row) => row.id === selectedId)
      if (index < 0) return
      event.preventDefault()
      const nextIndex = index + (event.key === 'ArrowDown' ? 1 : -1)
      const next = visible[nextIndex]
      if (!next) return
      const p = new URLSearchParams(params)
      p.set('node', next.id)
      setParams(p, { replace: true })
      const row = tableBody.current?.rows[nextIndex]
      row?.focus({ preventScroll: true })
      row?.scrollIntoView({ block: 'nearest' })
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [selectedId, visible, params, setParams])

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{PAGE_LABEL[kind]}{pipeline ? ` · ${pipelineName(pipeline)}` : ''}</h1>
            {data ? (
              <span className="barca-count" title={pipeline ?? undefined}>
                {rows.length} {kind === 'asset' ? 'assets' : kind === 'task' ? 'tasks' : 'sensors'}
              </span>
            ) : (
              <Skeleton width={64} height={14} />
            )}
          </div>
          <div className="barca-view-actions">
            {health?.read_only && <Tag tone="bare">read-only</Tag>}
            <ConnectionBadge connection={connection(health, healthError)} />
          </div>
        </div>
        <div className="barca-table-tools">
          <SearchInput
            placeholder="Search by name or file"
            value={filters.query}
            onChange={(query) => setFilters({ ...filters, query })}
            autoFocus
          />

          <Select
            label="Last run"
            value={filters.lastRun}
            options={LAST_RUNS.map((l) => ({
              value: l,
              label: LAST_RUN_LABEL[l] + (l === 'any' ? '' : ` (${counts.lastRun[l]})`),
            }))}
            onChange={(lastRun) => setFilters({ ...filters, lastRun })}
          />

          {(counts.scheduled > 0 || filters.scheduled) && (
            <ChipGroup label="Scheduled">
              <Chip
                pressed={filters.scheduled}
                onClick={() => setFilters({ ...filters, scheduled: !filters.scheduled })}
              >
                {counts.scheduled} scheduled
              </Chip>
            </ChipGroup>
          )}
          {(counts.partitioned > 0 || filters.partitioned) && (
            <ChipGroup label="Partitioned">
              <Chip
                pressed={filters.partitioned}
                onClick={() => setFilters({ ...filters, partitioned: !filters.partitioned })}
              >
                {counts.partitioned} partitioned
              </Chip>
            </ChipGroup>
          )}

          {isFiltered(filters) && (
            <button type="button" className="barca-clear" onClick={() => setFilters(EMPTY_FILTERS)}>
              <X size={12} /> Clear filters
            </button>
          )}
        </div>

        <div className="barca-table-tools">
          <ChipGroup label="State">
            {SEVERITIES.filter((sev) => counts.states[sev] > 0 || filters.states.includes(sev)).map((sev) => (
              <Chip
                key={sev}
                pressed={filters.states.includes(sev)}
                onClick={() => setFilters({ ...filters, states: toggle(filters.states, sev) })}
              >
                <StatusDot status={severityStatus(sev)} size={6} />
                {counts.states[sev]} {SEVERITY_LABEL[sev]}
              </Chip>
            ))}
          </ChipGroup>
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
          <div className="barca-table-skeleton" aria-label="Loading">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <Skeleton key={i} height={52} />
            ))}
          </div>
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
            <tbody ref={tableBody}>
              {visible.map((r) => (
                <tr
                  key={r.id}
                  tabIndex={0}
                  aria-selected={r.id === selectedId}
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true })
                    open(r)
                  }}
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
