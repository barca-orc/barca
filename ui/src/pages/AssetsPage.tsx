import { StreamNotice } from '@/components/StreamNotice'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { ArrowDown, ArrowUp, CalendarClock, ChevronDown, Clock, Layers, X } from 'lucide-react'
import {
  Button,
  Chip,
  ChipGroup,
  EmptyNote,
  MiddleName,
  PageRunButton,
  ResetDatabaseButton,
  RunningBadge,
  SearchInput,
  Select,
  Skeleton,
  StatusBadge,
  StatusGlyph,
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
import { useTimeZone } from '@/context/timeZone'
import { formatShort } from '@/lib/time'
import { useNodeRun } from '@/hooks/useNodeRun'
import { ApiError, metadataUnusable } from '@/lib/api'
import { BLOCKED_UPSTREAM, PINNED, PINNED_HINT, CACHE_STATES, LAST_ATTEMPT_FAILED, cacheStateGlyph, cacheStateLabel, nodeStateDisplay } from '@/lib/cacheStateDisplay'
import { blockedBy } from '@/lib/graphView'
import { shortName } from '@/lib/graph'
import { headline } from '@/lib/health'
import { plural } from '@/lib/plural'
import { isLive, liveGraphState } from '@/lib/graphState'
import { inPipeline, pipelineName } from '@/lib/pipeline'
import {
  EMPTY_FILTERS,
  LAST_RUNS,
  applyFilters,
  facetCounts,
  isFiltered,
  NEEDS_ATTENTION,
  NO_DEFAULTS,
  RECENT_MS,
  onDefaultStates,
  parseFilters,
  writeFilters,
  type Filters,
  type LastRun,
} from '@/lib/assetFilters'
import {
  DEFAULT_SORT,
  assetColumns,
  buildRows,
  fileDisplay,
  lastRunText,
  lastRunTitle,
  nextSort,
  parseSort,
  sortRows,
  tableLines,
  typicalText,
  type AssetRow,
  type Sort,
  type SortKey,
} from '@/lib/assetTable'
import type { NodeKind } from '@/lib/types'
import { PausedTag } from '@/components/assets/IncidentControls'
import { usePausedSet } from '@/hooks/usePauses'
import { usePinNotes } from '@/hooks/usePins'

const PAGE_LABEL: Record<NodeKind, string> = { asset: 'Assets', task: 'Tasks', sensor: 'Sensors' }

/** What a page with none of its kind says, in a sentence: what the kind is, and how to add one. */
const EMPTY_HINT: Record<NodeKind, ReactNode> = {
  asset: <>An asset is a cached function: decorate one with <code>@asset</code> in a served file and it appears here.</>,
  task: <>A task is a step that always runs, never cached (deploy, notify, migrate): decorate one with <code>@task</code> and run it with <code>barca run</code>.</>,
  sensor: <>A sensor observes something outside barca and says whether it changed: decorate one with <code>@sensor</code> and the assets downstream re-run when it does.</>,
}

const LAST_RUN_LABEL: Record<LastRun, string> = {
  any: 'Any time',
  hour: 'Last hour',
  day: 'Last 24h',
  week_plus: '7+ days ago',
  never: 'Not run',
}

/** Add or remove `v` from a multi-select list. */
function toggle<T>(list: T[], v: T): T[] {
  return list.includes(v) ? list.filter((x) => x !== v) : [...list, v]
}

/** A column header that sorts the table; the active one shows its direction. */
function SortHeader({
  label,
  column,
  sort,
  onSort,
  numeric = false,
  className,
  title,
}: {
  label: string
  column: SortKey
  sort: Sort
  onSort: (key: SortKey) => void
  numeric?: boolean
  className?: string
  title?: string
}) {
  const active = sort.key === column
  return (
    <th
      className={[numeric ? 'num' : null, className].filter(Boolean).join(' ') || undefined}
      title={title}
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
  const { data, isError, error, dataUpdatedAt, refetch } = useAssetStates()
  const { data: health } = useHealth()
  const { zone } = useTimeZone()
  const assetQuery = useAssets()
  const graph = useMemo(() => assetQuery.data ?? [], [assetQuery.data])
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
  // `?paused=1` (the paused banner's link): only the paused assets.
  const paused = usePausedSet()
  const onlyPaused = params.get('paused') === '1'
  // Which nodes a failure upstream keeps from running: judged over the whole graph, so a
  // pipeline's row is blocked by a failure in another one too.
  // Pinned assets read `pinned` (`GET /pins`), and pass no failure above them on.
  const pins = usePinNotes()
  const blockers = useMemo(() => blockedBy(data ?? [], graph, pins), [data, graph, pins])
  const rows = useMemo(
    () => buildRows((data ?? []).filter((n) => n.kind === kind && inPipeline(n.id, pipeline) && (!onlyPaused || paused.has(n.id))), dataUpdatedAt, blockers, pins),
    [data, dataUpdatedAt, pipeline, kind, onlyPaused, paused, blockers, pins],
  )
  // Filters live in the URL with the sort and pipeline, so a filtered view can
  // be reloaded and shared.
  const defaultStates = kind === 'task' ? NEEDS_ATTENTION : NO_DEFAULTS
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
  // What fails, and how much of the graph waits on it: the failing segment's tooltip.
  // The blocked count is the rows that read `blocked`, so the tooltip and the chip agree.
  const blocked = rows.filter((r) => !r.lastFailed && r.pinned === null && r.blockedBy).length
  // Rows under a header per file, which names the file once (each row's tooltip has its id),
  // unless one pipeline is open: the title names it then. A header folds its file's rows away.
  const [collapsed, setCollapsed] = useState<string[]>([])
  const grouped = !pipeline
  const lines = useMemo(() => tableLines(visible, grouped, new Set(collapsed)), [visible, grouped, collapsed])
  // Columns that say something on this page: Typical when a node takes 0.1s or more, Next run when one has a schedule.
  const columns = assetColumns(rows)
  const columnCount = 3 + Number(columns.typical) + Number(columns.next)
  const tableBody = useRef<HTMLTableSectionElement>(null)
  const scroller = useRef<HTMLDivElement>(null)
  const win = useTableWindow(lines.length, scroller, 36)
  const { scrollToIndex } = win

  // The node shown in the side panel (?node=<id>).
  const selectedId = params.get('node')
  const selectedNode = data?.find((n) => n.id === selectedId) ?? null
  const nodeRun = useNodeRun(selectedNode, { showTopbar: true, scopes: true })
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
      // The rows on screen, in order: a folded file's rows and the headers are stepped over.
      const index = lines.findIndex((line) => line.kind === 'row' && line.row.id === selectedId)
      if (index < 0) return
      event.preventDefault()
      const step = event.key === 'ArrowDown' ? 1 : -1
      let nextIndex = index + step
      while (lines[nextIndex]?.kind === 'file') nextIndex += step
      const next = lines[nextIndex]
      if (!next || next.kind !== 'row') return
      const p = new URLSearchParams(params)
      p.set('node', next.row.id)
      setParams(p, { replace: true })
      focusWindowRow(tableBody.current, nextIndex, scrollToIndex)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [selectedId, lines, params, setParams, scrollToIndex])

  return (
    <div className="barca-view">
      <StreamNotice notice={nodeRun.stream.notice} runId={nodeRun.run?.handle} />
      <div className="barca-view-head">
        {/* One toolbar: the title and count, the state filter, then search, the last-run filter,
            the view and Run. What fails needs no banner: its segment says how many, and what it blocks. */}
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{PAGE_LABEL[kind]}{pipeline ? ` · ${pipelineName(pipeline)}` : ''}</h1>
            {data ? (
              <span className="barca-count" title={pipeline ?? undefined}>
                {visible.length === rows.length ? rows.length.toLocaleString() : `${visible.length.toLocaleString()} of ${rows.length.toLocaleString()}`}
                <span className="barca-sr-only"> {rows.length === 1 ? kind : `${kind}s`}{visible.length === rows.length ? '' : ' shown'}</span>
              </span>
            ) : (
              <Skeleton width={24} height={14} />
            )}
          </div>
          {/* The state words, as `barca status` prints them; a failed last attempt is a filter of its own. */}
          <ChipGroup label="State" segmented>
            {(counts.failed > 0 || (filters.failed && !onDefault)) && (
              <Chip
                pressed={filters.failed}
                title={`${counts.failed} ${LAST_ATTEMPT_FAILED}${blocked > 0 ? ` · blocks ${plural(blocked, 'downstream node')}` : ''}`}
                onClick={() => setFilters({ ...filters, failed: !filters.failed })}
              >
                <StatusGlyph glyph="failed" size={12} />
                <b>{counts.failed}</b> <span className="barca-chip-word">{LAST_ATTEMPT_FAILED}</span>
              </Chip>
            )}
            {(counts.pinned > 0 || filters.pinned) && (
              <Chip pressed={filters.pinned} title={`${counts.pinned} ${PINNED}`} onClick={() => setFilters({ ...filters, pinned: !filters.pinned })}>
                <StatusGlyph glyph="pinned" size={12} />
                <b>{counts.pinned}</b> <span className="barca-chip-word">{PINNED}</span>
              </Chip>
            )}
            {(counts.blocked > 0 || (filters.blocked && !onDefault)) && (
              <Chip pressed={filters.blocked} title={`${counts.blocked} ${BLOCKED_UPSTREAM}`} onClick={() => setFilters({ ...filters, blocked: !filters.blocked })}>
                <StatusGlyph glyph="blocked" size={12} />
                <b>{counts.blocked}</b> <span className="barca-chip-word">{BLOCKED_UPSTREAM}</span>
              </Chip>
            )}
            {CACHE_STATES.filter((state) => counts.states[state] > 0 || (filters.states.includes(state) && !onDefault)).map((state) => (
              <Chip
                key={state}
                pressed={filters.states.includes(state)}
                title={`${counts.states[state]} ${cacheStateLabel(state)}`}
                onClick={() => setFilters({ ...filters, states: toggle(filters.states, state) })}
              >
                <StatusGlyph glyph={cacheStateGlyph(state)} size={12} />
                <b>{counts.states[state]}</b> <span className="barca-chip-word">{cacheStateLabel(state)}</span>
              </Chip>
            ))}
          </ChipGroup>
          {(counts.scheduled > 0 || filters.scheduled) && (
            <ChipGroup label="Scheduled">
              <Chip pressed={filters.scheduled} title={`${counts.scheduled} scheduled`} onClick={() => setFilters({ ...filters, scheduled: !filters.scheduled })}>
                <CalendarClock size={12} /><b>{counts.scheduled}</b> <span className="barca-chip-word">scheduled</span>
              </Chip>
            </ChipGroup>
          )}
          {(counts.partitioned > 0 || filters.partitioned) && (
            <ChipGroup label="Partitioned">
              <Chip pressed={filters.partitioned} title={`${counts.partitioned} partitioned`} onClick={() => setFilters({ ...filters, partitioned: !filters.partitioned })}>
                <Layers size={12} /><b>{counts.partitioned}</b> <span className="barca-chip-word">partitioned</span>
              </Chip>
            </ChipGroup>
          )}
          {isFiltered(filters) && rows.length > 0 && (
            <button type="button" className="barca-clear" onClick={() => setFilters(EMPTY_FILTERS)}>
              <X size={12} /> {onDefault ? `Show all ${rows.length.toLocaleString()}` : 'Clear'}
            </button>
          )}
          <div className="barca-view-actions">
            {nodeRun.run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(nodeRun.run.handle)}`}>View run</Link>}
            {health?.read_only && <Tag tone="bare">read-only</Tag>}
            {(rows.length > 0 || !data) && <SearchInput
              placeholder={`Search ${PAGE_LABEL[kind].toLowerCase()}`}
              label="Search by name or file"
              value={filters.query}
              onChange={(query) => setFilters({ ...filters, query })}
            />}
            {(rows.length > 0 || !data) && <Select
              label="Last run"
              icon={<Clock size={13} />}
              value={filters.lastRun}
              options={LAST_RUNS.map((l) => ({
                value: l,
                label: LAST_RUN_LABEL[l] + (l === 'any' ? '' : ` (${counts.lastRun[l]})`),
              }))}
              onChange={(lastRun) => setFilters({ ...filters, lastRun })}
            />}
            {kind === 'asset' && <ViewToggle />}
            <PageRunButton run={nodeRun.scopeRun} />
          </div>
        </div>
      </div>
      <RunNotice notice={nodeRun.notice} onDismiss={nodeRun.dismiss} />
      <div className="barca-assets-split">
      <div className="barca-view-body barca-table-scroll" ref={scroller}>
        {isError ? (
          <div className="barca-table-empty">
            <p>Can't load state: {error instanceof Error ? error.message : 'barca serve is not reachable'}{' '}
              <Button size="sm" onClick={() => void refetch()}>Retry</Button>
            </p>
            {error instanceof ApiError && error.remediation && <p>{error.remediation}</p>}
            {/* Starting over mends a database barca cannot use, and nothing else. */}
            {metadataUnusable(error) && <ResetDatabaseButton readOnly={health?.read_only ?? false} />}
          </div>
        ) : !data ? (
          <div className="barca-table-skeleton" aria-label="Loading">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <Skeleton key={i} height={32} />
            ))}
          </div>
        ) : rows.length === 0 && !onlyPaused ? (
          <EmptyNote title={`No ${PAGE_LABEL[kind].toLowerCase()}${pipeline ? ` in ${pipelineName(pipeline)}` : ''}`}>{EMPTY_HINT[kind]}</EmptyNote>
        ) : visible.length === 0 ? (
          <p className="barca-table-empty">{onDefault ? 'Nothing needs attention, and nothing ran in the last 24h.' : 'Nothing matches.'}</p>
        ) : (
          <table className="barca-table barca-asset-table">
            <colgroup>
              <col />
              <col className="col-state" />
              <col className="col-last" />
              {columns.typical && <col className="col-typical" />}
              {columns.next && <col className="col-next" />}
            </colgroup>
            <thead>
              <tr>
                <SortHeader label="Name" column="name" sort={sort} onSort={onSort} />
                <SortHeader label="State" column="state" sort={sort} onSort={onSort} className="col-state" />
                <SortHeader label="Last run" column="last" sort={sort} onSort={onSort} className="col-last" />
                {columns.typical && <SortHeader label="Typical" column="typical" sort={sort} onSort={onSort} numeric className="col-typical" title="Median of recent successful runs" />}
                {columns.next && <SortHeader label="Next run" column="next" sort={sort} onSort={onSort} className="col-next" />}
              </tr>
            </thead>
            <tbody ref={tableBody}>
              <TableSpacer height={win.before} columns={columnCount} />
              {win.items.map(({ index }) => {
                const line = lines[index]
                if (!line) return null
                if (line.kind === 'file') {
                  const { group, collapsed } = line
                  // The same derivation as the file's line in the sidebar (`fileDisplay`).
                  const shown = fileDisplay(group.rows)!
                  return (
                    <tr key={`file:${group.file}`} ref={win.measure} data-index={index} className="barca-file-head">
                      <td colSpan={columnCount}>
                        <button type="button" aria-expanded={!collapsed} title={collapsed ? `Show ${group.file}` : `Fold ${group.file}`} onClick={() => setCollapsed((folded) => toggle([...folded], group.file))}>
                          <ChevronDown size={13} />
                          <StatusGlyph glyph={shown.glyph} live={shown.live} size={12} label={shown.live ? shown.label : `worst: ${shown.label}`} />
                          <span className="barca-file-name">{group.file || '(no file)'}</span>
                          <span className="barca-count">{group.rows.length.toLocaleString()}</span>
                        </button>
                      </td>
                    </tr>
                  )
                }
                const r = line.row
                const liveStatus = nodeRun.statuses[r.id]
                return (
                <tr
                  key={r.id}
                  ref={win.measure}
                  data-index={index}
                  tabIndex={0}
                  aria-selected={r.id === selectedId}
                  data-failing={r.lastFailed || undefined}
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true })
                    open(r)
                  }}
                  onKeyDown={(e) => e.key === 'Enter' && open(r)}
                >
                  <td title={r.id}>
                    <div className="barca-cell-name">
                      <span className="name"><MiddleName name={labels.get(r.id) ?? r.name} title={r.id} /></span>
                      {/* Every row is of the page's kind (Assets, Tasks, Sensors): no tag says so. */}
                      <PausedTag id={r.id} paused={paused} />
                      {/* Why it is not fine, in a word or a line: what failed, what blocks it, what it is pinned to. */}
                      {r.lastFailed ? (
                        r.last?.error && <span className="barca-cell-why" title={r.last.error}>{headline(r.last.error)}</span>
                      ) : r.pinned !== null ? (
                        <span className="barca-cell-why" title={r.pinned}>{r.pinned}</span>
                      ) : r.blockedBy && (
                        <span className="barca-cell-why">
                          waits on{' '}
                          <button
                            type="button"
                            className="barca-link"
                            title={`Open ${r.blockedBy}`}
                            onClick={(event) => {
                              event.stopPropagation()
                              setSelected(r.blockedBy)
                            }}
                            onKeyDown={(event) => event.stopPropagation()}
                          >
                            {shortName(r.blockedBy)}
                          </button>
                        </span>
                      )}
                    </div>
                  </td>
                  {/* The tooltip starts with the state's word: a narrow table shows only its glyph. */}
                  <td className="col-state" title={r.running ? undefined : `${nodeStateDisplay(r).label}: ${r.pinned !== null && !r.lastFailed ? `${PINNED_HINT} ${r.pinned}` : r.stateHint}`}>
                    {liveStatus ? (
                      <StatusBadge glyph={liveGraphState(liveStatus).glyph} label={liveGraphState(liveStatus).label} live={isLive(liveGraphState(liveStatus))} size="sm" />
                    ) : r.running ? (
                      <RunningBadge running={r.running} nowMs={now} />
                    ) : (
                      <StatusBadge glyph={nodeStateDisplay(r).glyph} label={nodeStateDisplay(r).label} size="sm" />
                    )}
                  </td>
                  <td className="col-last" title={r.last ? lastRunTitle(r, now) : undefined}>
                    {lastRunText(r, now)}
                  </td>
                  {columns.typical && (
                    <td className="num col-typical" title={r.p95 ? `p95 ${r.p95}` : undefined}>
                      {typicalText(r)}
                    </td>
                  )}
                  {columns.next && <td className="col-next">{r.nextRunMs === null ? '–' : formatShort(r.nextRunMs, zone)}</td>}
                </tr>
                )
              })}
              <TableSpacer height={win.after} columns={columnCount} />
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
