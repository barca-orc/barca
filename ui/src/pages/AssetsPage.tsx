import { StreamNotice } from '@/components/StreamNotice'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { ArrowDown, ArrowUp, ChevronDown } from 'lucide-react'
import {
  Button,
  EmptyNote,
  MiddleName,
  ResetDatabaseButton,
  RunningBadge,
  Skeleton,
  StatusBadge,
  StatusGlyph,
  TableSpacer,
} from '@/components'
import { focusWindowRow, useTableWindow } from '@/hooks/useTableWindow'
import { NodePanel } from '@/components/assets/NodePanel'
import { RunNotice } from '@/components/RunNotice'
import { useHealth } from '@/hooks/useHealth'
import { useTimeZone } from '@/context/timeZone'
import { formatShort } from '@/lib/time'
import { useNodeRun } from '@/hooks/useNodeRun'
import { ApiError, metadataUnusable } from '@/lib/api'
import { PINNED_HINT, nodeStateDisplay } from '@/lib/cacheStateDisplay'
import { plural } from '@/lib/plural'
import { shortName } from '@/lib/graph'
import { headline } from '@/lib/health'
import { isLive, liveGraphState } from '@/lib/graphState'
import { pipelineName } from '@/lib/pipeline'
import {
  blockedKeys,
  DEFAULT_SORT,
  assetColumns,
  fileDisplay,
  lastRunText,
  lastRunTitle,
  nextSort,
  parseSort,
  tableLines,
  typicalText,
  type AssetRow,
  type Sort,
  type SortKey,
} from '@/lib/assetTable'
import type { NodeKind } from '@/lib/types'
import { PausedTag } from '@/components/assets/IncidentControls'
import { AssetsToolbar } from './AssetsToolbar'
import { PAGE_LABEL, toggle, useAssetFilter } from '@/hooks/useAssetFilter'

/** What a page with none of its kind says, in a sentence: what the kind is, and how to add one. */
const EMPTY_HINT: Record<NodeKind, ReactNode> = {
  asset: <>An asset is a cached function: decorate one with <code>@asset</code> in a served file and it appears here.</>,
  task: <>A task is a step that always runs, never cached (deploy, notify, migrate): decorate one with <code>@task</code> and run it with <code>barca run</code>.</>,
  sensor: <>A sensor observes something outside barca and says whether it changed: decorate one with <code>@sensor</code> and the assets downstream re-run when it does.</>,
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
  const { data: health } = useHealth()
  const { zone } = useTimeZone()
  const navigate = useNavigate()
  // The sort lives in the URL (#/assets?sort=typical&dir=desc), so it survives
  // a reload and can be shared.
  const [params, setParams] = useSearchParams()
  const sort = parseSort(params.get('sort'), params.get('dir'))
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
  // refreshes with every poll. The rows and filters are the graph view's too (`useAssetFilter`).
  const filter = useAssetFilter(kind, sort)
  const { data, dataUpdatedAt, isError, error, refetch } = filter.states
  const { graph, pipeline, paused, onlyPaused, rows, onDefault, visible, now } = filter
  // A name repeated across files reads as its id; two rows never show the same label.
  const nameCount = new Map<string, number>()
  for (const r of visible) nameCount.set(r.name, (nameCount.get(r.name) ?? 0) + 1)
  const labels = new Map(visible.filter((r) => nameCount.get(r.name)! > 1).map((r) => [r.id, r.id]))
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
        <AssetsToolbar
          filter={filter}
          run={nodeRun.scopeRun}
          extra={nodeRun.run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(nodeRun.run.handle)}`}>View run</Link>}
        />
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
                      {r.lastFailed && r.keys && r.keys.failed > 0 ? (
                        <span className="barca-cell-why">{r.keys.failed.toLocaleString()} of {plural(r.keys.total, 'key')} failed</span>
                      ) : r.lastFailed ? (
                        r.last?.error && <span className="barca-cell-why" title={r.last.error}>{headline(r.last.error)}</span>
                      ) : r.pinned !== null ? (
                        <span className="barca-cell-why" title={r.pinned}>{r.pinned}</span>
                      ) : !r.blockedBy && r.state === 'blocked' && r.keys && blockedKeys(r.stateHint) ? (
                        <span className="barca-cell-why">
                          {blockedKeys(r.stateHint)!.count.toLocaleString()} of {plural(r.keys.total, 'key')} wait on {blockedKeys(r.stateHint)!.upstream}
                        </span>
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
