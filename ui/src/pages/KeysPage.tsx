import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Play, X, ZoomOut } from 'lucide-react'
import { Button, ConfirmDialog, SearchInput, Select, Skeleton, StatusGlyph } from '@/components'
import { IssueList } from '@/components/partitions/IssueList'
import { KeyDetail } from '@/components/partitions/KeyDetail'
import { KeyHeatmap } from '@/components/partitions/KeyHeatmap'
import { KeyTable } from '@/components/partitions/KeyTable'
import { usePartitionKey, usePartitionKeysPages, usePartitionOverview } from '@/hooks/usePartitionGrid'
import { useWriteControl } from '@/hooks/useWriteControl'
import { usePausedSet } from '@/hooks/usePauses'
import { useRunDetail } from '@/hooks/useRuns'
import { runIsLive, runLabel } from '@/lib/runs'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { keysParams, keysQuery, keysSearch, NO_FILTER, toggleSort, withValues, type KeyFilter, type KeysParams } from '@/lib/keysPage'
import { KEY_STATES, keyStateDisplay } from '@/lib/keyStateDisplay'
import { alongValues, rangeBounds } from '@/lib/partitionGrid'
import { plural } from '@/lib/plural'
import { partitionSelection } from '@/lib/nodeDetail'
import type { KeyIssue } from '@/lib/types'

/** A dimension with at most this many values gets a select; a longer one is zoomed in the overview or searched. */
const SELECT_MAX = 60

/**
 * The key explorer (`#/keys?node=...`), what is wrong first: the asset's keys counted by state,
 * then each problem with where its keys are, then every key in one small overview, then the keys
 * themselves (those that need a run, unless all are asked for). The page does not scroll: the
 * table and the open key's detail do, each in its own place.
 *
 * Everything shown is served: the counts, the issues and the overview's rows by
 * `GET /assets/{id}/partitions/overview` (`barca status NODE --overview`), the keys a page at a
 * time by `.../partitions/keys` with the page's filters and order (`barca status NODE --keys`
 * with the same flags). A filter or an order asks the server again; nothing is decoded,
 * filtered or counted here.
 */
export function KeysPage() {
  const [search, setSearch] = useSearchParams()
  const params = keysParams(search)
  const { node, filter } = params
  const client = useQueryClient()
  // Runs are offered once the server is known to take them; none at all on a read-only one.
  const writes = useWriteControl()
  const readOnly = writes !== 'enabled'
  const paused = usePausedSet().has(node ?? '')
  const [runConfirm, setRunConfirm] = useState<{ node: string; partition: string } | null>(null)
  const overviewQuery = usePartitionOverview(node)
  const overview = overviewQuery.data
  const keyQuery = usePartitionKey(node, params.key)
  // Ages are read against when the overview arrived, as the asset tables read theirs.
  const nowMs = overviewQuery.dataUpdatedAt
  const along = useMemo(() => (overview ? alongValues(overview) : []), [overview])
  const zoom = rangeBounds(along, filter.range)
  const needing = overview?.needs_run ?? 0
  // The table: what the filters keep, and by default only what a run computes.
  const attention = !params.all && filter.states.length === 0 && needing > 0
  const query = overview
    ? keysQuery({ filter, sort: params.sort, needsRun: attention, along: overview.along, rangeValid: zoom !== null })
    : null
  const pages = usePartitionKeysPages(node, query, overview)
  const first = pages.data?.pages[0]
  const rows = useMemo(() => pages.data?.pages.flatMap((p) => p.keys) ?? [], [pages.data])
  const issues = overview?.issues ?? []
  const activeIssue = issues.find((i) => i.error === filter.error && filter.states.length === 1 && filter.states[0] === i.state) ?? null
  const { fetchNextPage } = pages
  const needMore = useCallback(() => { void fetchNextPage() }, [fetchNextPage])

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
    keysSearch({ node: id, key: params.key, filter: { ...NO_FILTER, values: filter.values, q: filter.q, range: filter.range }, all: false, sort: params.sort })
  const select = (key: string | null) => go(() => ({ key }))

  const start = useMutation({
    mutationFn: ({ target, partition, allowPaused = false }: { target: string; partition: string; allowPaused?: boolean }) =>
      api.startRun({ command: 'get', targets: [target], partition, ...(allowPaused ? { allow_paused: true } : {}) }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['runs'] })
      void client.invalidateQueries({ queryKey: ['partition-overview'] })
    },
  })

  const run = (partition: string | null) => {
    if (!node || !partition || readOnly) return
    if (paused) setRunConfirm({ node, partition })
    else start.mutate({ target: node, partition })
  }

  const retry = useRunDetail(start.data?.run_id ?? null)
  const retryStatus = retry.data?.run.status
  const retryId = start.data?.run_id
  useEffect(() => {
    if (retryId && retryStatus && !runIsLive(retryStatus)) {
      void client.invalidateQueries({ queryKey: ['partition-overview'] })
      void client.invalidateQueries({ queryKey: ['partition-keys-page'] })
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

  const keyCount = overview ? KEY_STATES.reduce((n, s) => n + overview.counts[s], 0) : 0
  const valueDims = overview?.dimensions.filter((d) => d.values.length > 1 && d.values.length <= SELECT_MAX) ?? []
  const otherDims = overview?.dimensions.filter((d) => d.name !== overview.along) ?? []
  // The primary action follows the same search, state, error and range as the table: the
  // server counts the keys to run among those it matched, and names them for the run.
  const scopeNeeding = first?.needs_run ?? 0
  const selection = first?.selection ?? null
  const shown = first?.total ?? 0

  return (
    <div className="barca-view barca-keys">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <Link className="barca-keys-back" to={`/assets?node=${encodeURIComponent(node)}`} title="Back to the asset">
              <ArrowLeft size={14} />
            </Link>
            <h1>{shortName(node)}</h1>
            {overview ? <span className="barca-count">{plural(keyCount, 'key')}</span> : <Skeleton width={60} height={14} />}
          </div>
          {overview && overview.chain.length > 1 && (
            <nav className="barca-keys-chain" aria-label="Assets sharing these keys">
              {overview.chain.map((id, i) => (
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
            {writes !== 'hidden' && <Button
              size="sm"
              iconLeft={<Play size={11} />}
              disabled={readOnly || scopeNeeding === 0 || selection === null}
              loading={start.isPending}
              title={scopeNeeding === 0
                ? 'Every key here is fresh'
                : selection === null
                  ? 'Too many scattered keys to name in one run: narrow the filter'
                  : 'Compute the keys that are not fresh; the rest stay cached'}
              onClick={() => run(selection)}
            >
              {scopeNeeding === 0 ? 'All fresh' : `Run ${plural(scopeNeeding, 'key')} shown`}
            </Button>}
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
          {overviewQuery.isError && <p className="barca-note">Couldn't load the keys: {(overviewQuery.error as Error).message}</p>}
          {overviewQuery.isLoading && <Skeleton height={240} />}
          {overview && !overview.known && (
            <p className="barca-note">The keys are not known yet: they come from an upstream result that has not been computed. Run the asset to compute it.</p>
          )}

          {/* 1. How many keys are in each state: the headline, and each a filter. */}
          {overview && keyCount > 0 && (
            <div className="barca-keys-counts" role="group" aria-label="Keys by state">
              {KEY_STATES.filter((s) => overview.counts[s] > 0).map((s) => (
                <button
                  key={s}
                  type="button"
                  className="barca-keys-count"
                  data-state={s}
                  aria-pressed={filter.states.length === 1 && filter.states[0] === s && filter.error === null}
                  onClick={() => setFilter((f) => (f.states.length === 1 && f.states[0] === s && f.error === null ? { ...f, states: [], error: null } : { ...f, states: [s], error: null }))}
                >
                  <span className="barca-keys-count-n">{overview.counts[s].toLocaleString()}</span>
                  <span className="barca-keys-count-word"><StatusGlyph glyph={keyStateDisplay(s).glyph} size={12} />{keyStateDisplay(s).word}</span>
                </button>
              ))}
            </div>
          )}

          {/* 2. What is wrong, and where. */}
          {overview && (
            <IssueList
              issues={issues}
              active={activeIssue}
              running={start.isPending}
              onShow={(issue: KeyIssue) => setFilter((f) => {
                const same = f.error === issue.error && f.states.length === 1 && f.states[0] === issue.state
                const state = KEY_STATES.find((s) => s === issue.state)
                return same || !state ? { ...f, states: [], error: null } : { ...f, states: [state], error: issue.error }
              })}
              onRun={readOnly ? null : (issue) => run(issue.selection)}
            />
          )}

          {/* 3. Every key in one picture; drag to zoom. */}
          {overview && overview.rows.length > 0 && (
            <section className="barca-keys-overview" aria-label="Every key by state">
              <div className="barca-keys-section-head">
                <h2>Overview</h2>
                {overview.along && (
                  <span className="barca-cell-sub">
                    {zoom
                      ? <>{overview.along} {along[zoom[0]]} → {along[zoom[1]]}</>
                      : <>{overview.rows.length > 1 ? `one row per ${otherDims.map((d) => d.name).join(' × ')}, ` : ''}{overview.along} left to right · drag to zoom</>}
                    {overview.truncated && <> · the first {overview.rows.length.toLocaleString()} of {overview.rows_total.toLocaleString()} rows</>}
                  </span>
                )}
                {zoom && (
                  <Button variant="ghost" size="sm" iconLeft={<ZoomOut size={12} />} onClick={() => setFilter((f) => ({ ...f, range: null }))}>
                    Show all
                  </Button>
                )}
              </div>
              <KeyHeatmap
                overview={overview}
                window={zoom}
                onZoom={(a, b) => setFilter((f) => ({ ...f, range: { from: along[a]!, to: along[b]! } }))}
                onOpen={(key) => select(key)}
              />
            </section>
          )}

          {/* 4. The keys: what needs a run, or all of them. */}
          {overview && (
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
                <span className="barca-cell-sub">{!first || shown === keyCount ? '' : `${plural(shown, 'key')} shown`}</span>
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
              {pages.isError && <p className="barca-note">Couldn't load the keys: {(pages.error as Error).message}</p>}
              {first ? (
                <KeyTable
                  dimensions={first.dimensions}
                  rows={rows}
                  total={first.total}
                  selected={params.key}
                  onSelect={select}
                  onNeedMore={needMore}
                  loadingMore={pages.isFetchingNextPage}
                  sort={params.sort}
                  onSort={(pressed) => go((p) => ({ sort: toggleSort(p.sort, pressed) }))}
                  query={query ?? ''}
                  stale={pages.isPlaceholderData}
                  nowMs={nowMs}
                />
              ) : !pages.isError && <Skeleton height={120} />}
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
