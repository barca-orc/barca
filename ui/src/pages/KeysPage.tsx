import { useEffect, useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Play, X, ZoomOut } from 'lucide-react'
import { Button, ConfirmDialog, SearchInput, Select, Skeleton, StatusGlyph } from '@/components'
import { IssueList } from '@/components/partitions/IssueList'
import { KeyDetail } from '@/components/partitions/KeyDetail'
import { KeyHeatmap } from '@/components/partitions/KeyHeatmap'
import { KeyTable } from '@/components/partitions/KeyTable'
import { usePartitionGrid, usePartitionKey } from '@/hooks/usePartitionGrid'
import { useHealth } from '@/hooks/useHealth'
import { usePausedSet } from '@/hooks/usePauses'
import { useRunDetail } from '@/hooks/useRuns'
import { runIsLive, runLabel } from '@/lib/runs'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { keysParams, keysSearch, withValues, type KeysParams } from '@/lib/keysPage'
import {
  cellOf, countStates, filterCells, exactSelection, gridOf, issueFilter, issueGroups, issueSelection, keyOf, KEY_STATE_GLYPH,
  KEY_STATE_WORD, KEY_STATES, layoutBlocks, NO_FILTER, rangeBounds, type IssueGroup, type KeyFilter, type KeyState,
} from '@/lib/partitionGrid'
import { plural } from '@/lib/plural'
import { partitionSelection } from '@/lib/nodeDetail'

/** The states the table shows by default when any key has one: what needs attention. */
const ATTENTION: readonly KeyState[] = ['failed', 'blocked', 'stale', 'never_run']

/** A dimension with at most this many values gets a select; a longer one is zoomed in the overview or searched. */
const SELECT_MAX = 60

/** Keys a run computes: every state but fresh. */
function countNeeding(counts: Record<KeyState, number>): number {
  return ATTENTION.reduce((n, s) => n + counts[s], 0)
}

/**
 * The key explorer (`#/keys?node=...`), what is wrong first: the asset's keys counted by state,
 * then each problem with where its keys are, then every key in one small overview, then the keys
 * themselves (those that need attention, unless all are asked for). The page does not scroll:
 * the table and the open key's detail do, each in its own place.
 */
export function KeysPage() {
  const [search, setSearch] = useSearchParams()
  const params = keysParams(search)
  const { node, filter } = params
  const client = useQueryClient()
  const readOnly = useHealth().data?.read_only ?? true
  const paused = usePausedSet().has(node ?? '')
  const [runConfirm, setRunConfirm] = useState<{ node: string; partition: string } | null>(null)
  const gridQuery = usePartitionGrid(node)
  const keyQuery = usePartitionKey(node, params.key)
  const grid = useMemo(() => (gridQuery.data ? gridOf(gridQuery.data) : null), [gridQuery.data])
  // Ages are read against when the grid arrived, as the asset tables read theirs.
  const nowMs = gridQuery.dataUpdatedAt
  const layout = useMemo(() => (grid ? layoutBlocks(grid) : null), [grid])
  const totals = useMemo(() => (grid ? countStates(grid, null) : null), [grid])
  const issues = useMemo(() => (grid ? issueGroups(grid) : []), [grid])
  const needing = totals ? countNeeding(totals) : 0
  // The table: what the filters keep, and by default only what needs attention.
  const attention = !params.all && filter.states.length === 0 && needing > 0
  const tableFilter: KeyFilter = attention ? { ...filter, states: ATTENTION } : filter
  const filterKey = `${keysSearch({ node: null, key: null, filter: tableFilter, all: false })}`
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const cells = useMemo(() => (grid ? filterCells(grid, tableFilter) : []), [grid, filterKey])
  const selected = grid && params.key ? cellOf(grid, params.key) : null
  const zoom = grid ? rangeBounds(grid, filter.range) : null
  const activeIssue = issues.find((i) => {
    const f = issueFilter(i)
    return f.error === filter.error && filter.states.length === 1 && filter.states[0] === f.states[0]
  }) ?? null

  // Each change is read against the URL as it is now, not as it was when this page last drew:
  // two changes in a row (a chip, then a select) both land.
  const go = (change: (p: KeysParams) => Partial<KeysParams>) => {
    const hash = window.location.hash
    const now = keysParams(new URLSearchParams(hash.includes('?') ? hash.slice(hash.indexOf('?') + 1) : ''))
    setSearch(keysSearch({ ...now, ...change(now) }), { replace: true })
  }
  const setFilter = (change: (f: KeyFilter) => KeyFilter) => go((p) => ({ filter: change(p.filter) }))
  // Another node of the chain, at the same key: its own states and errors, so only the key, the
  // dimension values, the range and the search carry over.
  const chainSearch = (id: string) =>
    keysSearch({ node: id, key: params.key, filter: { ...NO_FILTER, values: filter.values, q: filter.q, range: filter.range }, all: false })
  const select = (cell: number | null) => go(() => ({ key: cell === null || !grid ? null : keyOf(grid, cell) }))

  const start = useMutation({
    mutationFn: ({ target, partition, allowPaused = false }: { target: string; partition: string; allowPaused?: boolean }) =>
      api.startRun({ command: 'get', targets: [target], partition, ...(allowPaused ? { allow_paused: true } : {}) }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['runs'] })
      void client.invalidateQueries({ queryKey: ['partition-grid'] })
    },
  })

  const run = (partition: string | null) => {
    if (!node || !partition || readOnly) return
    if (paused) setRunConfirm({ node, partition })
    else start.mutate({ target: node, partition })
  }

  const retry = useRunDetail(start.data?.run_id ?? null)
  const retryStatus = retry.data?.status
  const retryId = start.data?.run_id
  useEffect(() => {
    if (retryId && retryStatus && !runIsLive(retryStatus)) {
      void client.invalidateQueries({ queryKey: ['partition-grid'] })
      void client.invalidateQueries({ queryKey: ['partition-key'] })
    }
  }, [client, retryId, retryStatus])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && (e.target as HTMLElement | null)?.tagName !== 'INPUT' && params.key) go(() => ({ key: null }))
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  if (!node) {
    return <div className="barca-view"><p className="barca-note">Pick a partitioned asset to explore its keys.</p></div>
  }

  const raw = gridQuery.data
  const keyCount = totals ? KEY_STATES.reduce((n, s) => n + totals[s], 0) : 0
  const valueDims = grid?.dims.filter((d) => d.values.length > 1 && d.values.length <= SELECT_MAX) ?? []
  const along = grid && layout ? grid.dims[layout.along] : null
  // The primary action follows the same search, state, error and range as the table.
  const scopeNeeding = grid ? countNeeding(countStates(grid, cells)) : 0
  const selection = grid ? exactSelection(grid, cells) : null

  return (
    <div className="barca-view barca-keys">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <Link className="barca-keys-back" to={`/assets?node=${encodeURIComponent(node)}`} title="Back to the asset">
              <ArrowLeft size={14} />
            </Link>
            <h1>{shortName(node)}</h1>
            {grid ? <span className="barca-count">{plural(keyCount, 'key')}</span> : <Skeleton width={60} height={14} />}
          </div>
          {raw && raw.chain.length > 1 && (
            <nav className="barca-keys-chain" aria-label="Assets sharing these keys">
              {raw.chain.map((id, i) => (
                <span key={id} className="barca-keys-chain-step">
                  {i > 0 && <span aria-hidden className="barca-keys-chain-arrow">→</span>}
                  {id === node
                    ? <b aria-current="page">{shortName(id)}</b>
                    : <Link to={`/keys?${chainSearch(id)}`}>{shortName(id)}</Link>}
                </span>
              ))}
            </nav>
          )}
          <div className="barca-view-actions">
            <Button
              size="sm"
              iconLeft={<Play size={11} />}
              disabled={readOnly || scopeNeeding === 0 || selection === null}
              loading={start.isPending}
              title={readOnly ? 'This server is read-only' : scopeNeeding === 0 ? 'Every key here is fresh' : 'Compute the keys that are not fresh; the rest stay cached'}
              onClick={() => run(selection)}
            >
              {scopeNeeding === 0 ? 'All fresh' : `Run ${plural(scopeNeeding, 'key')} shown`}
            </Button>
          </div>
        </div>
        {(start.data || start.error) && (
          <p className="barca-keys-notice" role="status">
            {start.error
              ? `Couldn't start the run: ${(start.error as Error).message}`
              : <>{retryStatus ? `Run ${runLabel(retryStatus)}` : 'Started run'} <Link to={`/runs?run=${encodeURIComponent(start.data!.run_id)}`}>{start.data!.run_id}</Link></>}
          </p>
        )}
      </div>

      {runConfirm && <ConfirmDialog
        title={`Run ${shortName(runConfirm.node)} while it is paused?`}
        body="Scheduled runs skip this asset until resumed. This runs only the selected keys now; the asset stays paused."
        action="Run anyway"
        onCancel={() => setRunConfirm(null)}
        onConfirm={() => { start.mutate({ target: runConfirm.node, partition: runConfirm.partition, allowPaused: true }); setRunConfirm(null) }}
      />}
      <div className="barca-keys-body">
        <div className="barca-keys-main">
          {gridQuery.isError && <p className="barca-note">Couldn't load the keys: {(gridQuery.error as Error).message}</p>}
          {gridQuery.isLoading && <Skeleton height={240} />}
          {raw && !raw.known && (
            <p className="barca-note">The keys are not known yet: they come from an upstream result that has not been computed. Run the asset to compute it.</p>
          )}

          {/* 1. How many keys are in each state: the headline, and each a filter. */}
          {totals && keyCount > 0 && (
            <div className="barca-keys-counts" role="group" aria-label="Keys by state">
              {KEY_STATES.filter((s) => totals[s] > 0).map((s) => (
                <button
                  key={s}
                  type="button"
                  className="barca-keys-count"
                  data-state={s}
                  aria-pressed={filter.states.length === 1 && filter.states[0] === s && filter.error === null}
                  onClick={() => setFilter((f) => (f.states.length === 1 && f.states[0] === s && f.error === null ? { ...f, states: [], error: null } : { ...f, states: [s], error: null }))}
                >
                  <span className="barca-keys-count-n">{totals[s].toLocaleString()}</span>
                  <span className="barca-keys-count-word"><StatusGlyph glyph={KEY_STATE_GLYPH[s]} size={12} />{KEY_STATE_WORD[s]}</span>
                </button>
              ))}
            </div>
          )}

          {/* 2. What is wrong, and where. */}
          {grid && (
            <IssueList
              issues={issues}
              active={activeIssue}
              running={start.isPending}
              onShow={(issue: IssueGroup) => setFilter((f) => {
                const next = issueFilter(issue)
                const same = f.error === next.error && f.states.length === 1 && f.states[0] === next.states[0]
                return same ? { ...f, states: [], error: null } : { ...f, ...next }
              })}
              onRun={readOnly ? null : (issue) => run(issueSelection(grid, issue))}
            />
          )}

          {/* 3. Every key in one picture; drag to zoom. */}
          {grid && layout && layout.blocks.length > 0 && (
            <section className="barca-keys-overview" aria-label="Every key by state">
              <div className="barca-keys-section-head">
                <h2>Overview</h2>
                {along && (
                  <span className="barca-cell-sub">
                    {zoom
                      ? <>{along.name} {along.values[zoom[0]]} → {along.values[zoom[1]]}</>
                      : <>{layout.blocks.length > 1 ? `one row per ${grid.dims.filter((_, i) => i !== layout.along).map((d) => d.name).join(' × ')}, ` : ''}{along.name} left to right · drag to zoom</>}
                  </span>
                )}
                {zoom && (
                  <Button variant="ghost" size="sm" iconLeft={<ZoomOut size={12} />} onClick={() => setFilter((f) => ({ ...f, range: null }))}>
                    Show all
                  </Button>
                )}
              </div>
              <KeyHeatmap
                grid={grid}
                layout={layout}
                window={zoom}
                onZoom={(a, b) => setFilter((f) => ({ ...f, range: { from: along!.values[a]!, to: along!.values[b]! } }))}
                onOpen={(cell) => select(cell)}
              />
            </section>
          )}

          {/* 4. The keys: what needs attention, or all of them. */}
          {grid && (
            <section className="barca-keys-table" aria-label="Keys">
              <div className="barca-keys-section-head">
                <div className="barca-seg" role="group" aria-label="Which keys">
                  <button type="button" aria-pressed={attention || (filter.states.length > 0 && !params.all)} disabled={needing === 0}
                    onClick={() => go((p) => ({ all: false, filter: { ...p.filter, states: [], error: null } }))}>
                    Needs attention <b>{needing.toLocaleString()}</b>
                  </button>
                  <button type="button" aria-pressed={params.all && filter.states.length === 0}
                    onClick={() => go((p) => ({ all: true, filter: { ...p.filter, states: [], error: null } }))}>
                    All <b>{keyCount.toLocaleString()}</b>
                  </button>
                </div>
                <span className="barca-cell-sub">{cells.length === keyCount ? '' : `${plural(cells.length, 'key')} shown`}</span>
                <div className="barca-keys-list-tools">
                  {valueDims.map((d) => (
                    <Select
                      key={d.name}
                      label={d.name}
                      value={filter.values[d.name]?.[0] ?? ''}
                      options={[{ value: '', label: `Any ${d.name}` }, ...d.values.map((v) => ({ value: v, label: v }))]}
                      onChange={(v) => setFilter((f) => withValues(f, d.name, v ? [v] : []))}
                    />
                  ))}
                  <SearchInput value={filter.q} onChange={(q) => setFilter((f) => ({ ...f, q }))} placeholder="Search keys" />
                  {(filter.q || filter.range || Object.values(filter.values).some((v) => v.length > 0) || filter.error !== null) && (
                    <Button variant="ghost" size="sm" iconLeft={<X size={12} />} onClick={() => setFilter(() => NO_FILTER)}>Clear</Button>
                  )}
                </div>
              </div>
              <KeyTable grid={grid} cells={cells} selected={selected} onSelect={select} nowMs={nowMs} />
            </section>
          )}
        </div>
        {params.key && (
          <KeyDetail
            node={node}
            keyName={params.key}
            detail={keyQuery.data}
            loading={keyQuery.isLoading}
            error={keyQuery.error as Error | null}
            hrefFor={(id) => `/keys?${chainSearch(id)}`}
            running={start.isPending}
            onRun={readOnly ? null : () => run(partitionSelection(params.key!))}
            onClose={() => go(() => ({ key: null }))}
          />
        )}
      </div>
    </div>
  )
}
