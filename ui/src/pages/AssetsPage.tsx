import { StreamNotice } from '@/components/StreamNotice'
import { useEffect, useMemo, useRef } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { ArrowDown, ArrowUp, X } from 'lucide-react'
import {
  Chip,
  ChipGroup,
  ErrorLine,
  LastSuccess,
  MiddleName,
  ResetDatabaseButton,
  RunningBadge,
  SearchInput,
  Select,
  Skeleton,
  StatusBadge,
  StatusDot,
  TableSpacer,
  Tag,
  ViewToggle,
} from '@/components'
import { focusWindowRow, useTableWindow } from '@/hooks/useTableWindow'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useAssets } from '@/hooks/useAssets'
import { NodePanel } from '@/components/assets/NodePanel'
import { RunNotice } from '@/components/RunNotice'
import { useHealth } from '@/hooks/useHealth'
import { useNodeRun } from '@/hooks/useNodeRun'
import { ApiError, metadataUnusable } from '@/lib/api'
import { liveGraphState } from '@/lib/graphState'
import { nodeKindLabel, severityWord } from '@/lib/status'
import { inPipeline, pipelineName } from '@/lib/pipeline'
import {
  EMPTY_FILTERS,
  LAST_RUNS,
  applyFilters,
  facetCounts,
  isFiltered,
  NEEDS_ATTENTION,
  RECENT_MS,
  onDefaultStates,
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
  type Sort,
  type SortKey,
} from '@/lib/assetTable'
import type { NodeKind } from '@/lib/types'

const PAGE_LABEL: Record<NodeKind, string> = { asset: 'Assets', task: 'Tasks', sensor: 'Sensors' }

const LAST_RUN_LABEL: Record<LastRun, string> = {
  any: 'any time',
  hour: 'last hour',
  day: 'last 24h',
  week_plus: '7+ days ago',
  never: 'not run',
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
  const { data: health } = useHealth()
  const assetQuery = useAssets()
  const graph = useMemo(() => (assetQuery.isPlaceholderData ? [] : assetQuery.data ?? []), [assetQuery.data, assetQuery.isPlaceholderData])
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
  const defaultStates = kind === 'task' ? NEEDS_ATTENTION : []
  const filters = { ...parseFilters(params, defaultStates), kinds: [] }
  const onDefault = onDefaultStates(filters, defaultStates)
  const setFilters = (f: Filters) => setParams(writeFilters(params, f, defaultStates), { replace: true })
  const now = dataUpdatedAt
  // The default also shows what ran recently: a task that just ran is what the page is for.
  const opts = onDefault ? { recentMs: RECENT_MS } : {}
  const counts = facetCounts(rows, filters, now, opts)
  const visible = sortRows(applyFilters(rows, filters, now, opts), sort)
  // A name repeated across files reads as its id; two rows never show the same label.
  const nameCount = new Map<string, number>()
  for (const r of visible) nameCount.set(r.name, (nameCount.get(r.name) ?? 0) + 1)
  const labels = new Map(visible.filter((r) => nameCount.get(r.name)! > 1).map((r) => [r.id, r.id]))
  const tableBody = useRef<HTMLTableSectionElement>(null)
  const scroller = useRef<HTMLDivElement>(null)
  const win = useTableWindow(visible.length, scroller)
  const { scrollToIndex } = win

  // The node shown in the side panel (?node=<id>).
  const selectedId = params.get('node')
  const selectedNode = data?.find((n) => n.id === selectedId) ?? null
  const nodeRun = useNodeRun(selectedNode, { showTopbar: kind === 'asset' || selectedNode !== null, scopes: kind === 'asset' })
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
      focusWindowRow(tableBody.current, nextIndex, scrollToIndex)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [selectedId, visible, params, setParams, scrollToIndex])

  return (
    <div className="barca-view">
      <StreamNotice notice={nodeRun.stream.notice} runId={nodeRun.run?.handle} />
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
            <ChipGroup label="State">
              {SEVERITIES.filter((sev) => counts.states[sev] > 0 || (filters.states.includes(sev) && !onDefault)).map((sev) => (
                <Chip
                  key={sev}
                  pressed={filters.states.includes(sev)}
                  onClick={() => setFilters({ ...filters, states: toggle(filters.states, sev) })}
                >
                  <StatusDot status={severityStatus(sev)} size={6} />
                  {counts.states[sev]} {severityWord(sev)}
                </Chip>
              ))}
            </ChipGroup>
          </div>
          <div className="barca-view-actions">
            {nodeRun.run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(nodeRun.run.handle)}`}>View run</Link>}
            {health?.read_only && <Tag tone="bare">read-only</Tag>}
            {kind === 'asset' && <ViewToggle />}
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
              <X size={12} /> {onDefault ? `Show all ${rows.length.toLocaleString()}` : 'Clear filters'}
            </button>
          )}
          <span className="barca-count barca-shown">
            {visible.length === rows.length ? `${rows.length} shown` : `${visible.length} of ${rows.length} shown`}
          </span>
        </div>
      </div>

      <RunNotice notice={nodeRun.notice} onDismiss={nodeRun.dismiss} />
      <div className="barca-assets-split">
      <div className="barca-view-body barca-table-scroll" ref={scroller}>
        {isError ? (
          <div className="barca-table-empty">
            <p>Can't load state: {error instanceof Error ? error.message : 'barca serve is not reachable'}</p>
            {error instanceof ApiError && error.remediation && <p>{error.remediation}</p>}
            {/* Starting over mends a database barca cannot use, and nothing else. */}
            {metadataUnusable(error) && <ResetDatabaseButton readOnly={health?.read_only ?? false} />}
          </div>
        ) : !data ? (
          <div className="barca-table-skeleton" aria-label="Loading">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <Skeleton key={i} height={52} />
            ))}
          </div>
        ) : visible.length === 0 ? (
          <p className="barca-table-empty">{onDefault ? 'Nothing needs attention, and nothing ran in the last 24h.' : 'Nothing matches.'}</p>
        ) : (
          <table className="barca-table">
            <thead>
              <tr>
                <SortHeader label="Name" column="name" sort={sort} onSort={onSort} />
                <SortHeader label="State" column="state" sort={sort} onSort={onSort} />
                <SortHeader label="Last run" column="last" sort={sort} onSort={onSort} />
                <SortHeader label="Last success" column="success" sort={sort} onSort={onSort} />
                <SortHeader label="Typical" column="typical" sort={sort} onSort={onSort} numeric />
                <SortHeader label="Next run" column="next" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody ref={tableBody}>
              <TableSpacer height={win.before} columns={6} />
              {win.items.map(({ index }) => {
                const r = visible[index]
                if (!r) return null
                const liveStatus = nodeRun.statuses[r.id]
                return (
                <tr
                  key={r.id}
                  ref={win.measure}
                  data-index={index}
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
                      <span className="name"><MiddleName name={labels.get(r.id) ?? r.name} title={r.id} /></span>
                      {r.kind !== 'asset' && <Tag size="sm">{nodeKindLabel(r)}</Tag>}
                    </div>
                    <div className="barca-cell-sub">{r.file}</div>
                  </td>
                  <td title={r.running ? undefined : r.stateHint}>
                    {liveStatus ? (
                      <StatusBadge status={liveStatus} label={liveGraphState(liveStatus).label} />
                    ) : r.running ? (
                      <RunningBadge running={r.running} nowMs={now} />
                    ) : (
                      <StatusBadge status={severityStatus(r.severity)} label={r.stateLabel} size="sm" />
                    )}
                  </td>
                  <td>
                    {r.last ? (
                      <>
                        <div>{r.last.status} · {r.last.ago}</div>
                        {r.last.error && <div className="barca-cell-error"><ErrorLine error={r.last.error} /></div>}
                      </>
                    ) : (
                      <span className="barca-cell-sub">not run</span>
                    )}
                  </td>
                  <td>
                    <LastSuccess at={r.lastSuccessAt} failingSince={r.failingSince} failures={r.failures} nowMs={now} />
                  </td>
                  <td className="num" title={r.p95 ? `p95 ${r.p95}` : undefined}>
                    {r.typical ?? '–'}
                  </td>
                  <td>{formatNextRun(r.nextRunMs)}</td>
                </tr>
                )
              })}
              <TableSpacer height={win.after} columns={6} />
            </tbody>
          </table>
        )}
      </div>
      {selectedNode && (
        <NodePanel
          runAction={nodeRun.action}
          node={selectedNode}
          nodes={data ?? []}
          graph={graph}
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
