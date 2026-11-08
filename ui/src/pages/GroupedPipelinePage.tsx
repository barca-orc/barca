import { StreamNotice } from '@/components/StreamNotice'
import { useDeferredValue, useEffect, useMemo, useRef } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { match } from 'ts-pattern'
import { ArrowLeft, ArrowDown, ArrowRight, ChevronRight, FolderOpen, Maximize } from 'lucide-react'
import { Button, HealthBar, IconButton, PageRunButton, ResetDatabaseButton, SearchInput, Select, StatusGlyph, ViewToggle } from '@/components'
import { focusWindowRow, useTableWindow } from '@/hooks/useTableWindow'
import { GraphCanvas, type GraphCanvasHandle } from '@/components/graph/GraphCanvas'
import { NodePanel } from '@/components/assets/NodePanel'
import { RunNotice } from '@/components/RunNotice'
import { useGroups } from '@/hooks/useGroups'
import { useAssets } from '@/hooks/useAssets'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useHealth } from '@/hooks/useHealth'
import { useNodeRun } from '@/hooks/useNodeRun'
import { useProgram } from '@/hooks/useProgram'
import { usePartitionErrors } from '@/hooks/usePartitionErrors'
import { api, ApiError, metadataUnusable } from '@/lib/api'
import { INITIAL_FAILURES, failureTable, updateFailures, type FailuresEffect } from '@/lib/failuresView'
import { PENDING_GRAPH_STATE } from '@/lib/graphState'
import { failedKeyNodes, failures } from '@/lib/health'
import { distinctNames, ENTRY_SORTS, groupHealth, groupPath, groupTree, plural } from '@/lib/groups'
import {
  SORT_LABEL,
  applyChanges,
  dirChanges,
  groupedLevel,
  modeChanges,
  nextRowIndex,
  openGroupChanges,
  parseGroupedParams,
  revealChanges,
  rootName,
  selectChanges,
  visibleFailures,
  type ParamChanges,
} from '@/lib/groupedPage'
import { runningCount } from '@/lib/running'
import { shortName } from '@/lib/graph'
import { healthCounts } from '@/lib/health'
import { FailuresView } from './FailuresView'
import { GroupedTable, GroupPanel, LineagePicker } from './GroupedView'
import { usePinNotes } from '@/hooks/usePins'

/** The hash route's query, as it is now (`#/assets?pipeline=…`). */
function currentParams(): URLSearchParams {
  return new URLSearchParams(window.location.hash.split('?')[1] ?? '')
}

/**
 * A pipeline (or every pipeline) by its groups: the open level as a table or a graph, the
 * failures in scope, and the selected group's or node's panel. This component only fetches,
 * runs the URL and run effects and composes the views: what is shown is decided in
 * `lib/groupedPage.ts`.
 */
export function GroupedPipelinePage() {
  const [params, setParams] = useSearchParams()
  const pins = usePinNotes()
  const groupsQuery = useGroups()
  const assetQuery = useAssets()
  const stateQuery = useAssetStates()
  const healthQuery = useHealth()
  const pipeline = params.get('pipeline')
  const tree = useMemo(() => groupTree((groupsQuery.data ?? []).filter((g) => !pipeline || g.id.startsWith(`group:${pipeline}:`))), [groupsQuery.data, pipeline])
  const view = useMemo(() => parseGroupedParams(params, tree), [params, tree])
  const { graph, flat, show, selected, sort, dir } = view
  // Typing stays ahead of the list: a one-letter search can match thousands of nodes.
  const query = useDeferredValue(view.query)
  const allAssets = useMemo(() => assetQuery.data ?? [], [assetQuery.data])
  const states = useMemo(() => stateQuery.data ?? [], [stateQuery.data])
  const selectedAsset = allAssets.find((a) => a.id === selected) ?? null
  const nodeRun = useNodeRun(selectedAsset)
  const live = nodeRun.statuses
  const level = useMemo(
    () => groupedLevel({ params: { ...view, query }, tree, allAssets, states, statesFailed: stateQuery.isError, live }),
    [view, query, tree, allAssets, states, stateQuery.isError, live],
  )
  const { assets, byId, leaves, sole, scope, entries, map, tooBig, lineageOf, drawn, visualStates, scopeIds, scopedStates, visibleEntries, isHit, pathOf, placeOf } = level
  const canvas = useRef<GraphCanvasHandle>(null)
  const tableBody = useRef<HTMLTableSectionElement>(null)
  const scroller = useRef<HTMLDivElement>(null)
  const navigate = useNavigate()

  const scopeHealth = useMemo(() => healthCounts(scopeIds, byId, live), [scopeIds, byId, live])
  const graphKnown = !!assetQuery.data
  const keyErrors = usePartitionErrors(useMemo(() => failedKeyNodes(scopedStates), [scopedStates]))
  const failureList = useMemo(() => failures(scopedStates, graphKnown ? level.consumers : null, keyErrors), [scopedStates, graphKnown, level.consumers, keyErrors])
  const shownFailures = useMemo(() => visibleFailures(failureList, query), [failureList, query])

  const [failuresModel, failuresDispatch] = useProgram(
    INITIAL_FAILURES,
    updateFailures,
    (effect: FailuresEffect, dispatch) => {
      const failed = (error: unknown) => dispatch({ type: 'action_failed', text: error instanceof Error ? error.message : String(error) })
      match(effect)
        // The shared run line follows it: the run id, its progress and its outcome.
        .with({ type: 'retry' }, ({ id, kind }) => nodeRun.retry({ id, kind }))
        .with({ type: 'copy' }, ({ text }) => {
          navigator.clipboard.writeText(text).then(() => dispatch({ type: 'done', text: 'Error copied' }), failed)
        })
        .with({ type: 'open_run' }, ({ id, name, runId }) => {
          if (runId) {
            navigate(`/runs?run=${encodeURIComponent(runId)}`)
            return
          }
          api.asset(id).then(({ stats }) => {
            const run = stats.recent_runs.find((r) => r.status !== 'success')?.run_id
            if (run) navigate(`/runs?run=${encodeURIComponent(run)}`)
            else dispatch({ type: 'action_failed', text: `No run is recorded for ${name}` })
          }, failed)
        })
        .exhaustive()
    },
  )
  const { rows: failureRows } = failureTable(shownFailures)
  const rowCount = show === 'failures' ? failureRows.length : visibleEntries.length
  // Null for a row that cannot be selected: a cause's heading.
  const rowIds = show === 'failures' ? failureRows.map((r) => (r.kind === 'failure' ? r.failure.id : null)) : visibleEntries.map((e) => e.id)
  // What each row is called in this table, so no two read the same.
  const leafIds = show === 'failures' ? shownFailures.map((f) => f.id) : visibleEntries.filter((e) => e.kind !== 'group').map((e) => e.id)
  const labels = distinctNames(leafIds)
  const win = useTableWindow(rowCount, scroller)
  const crumbScope = scope === sole ? null : scope
  const path = groupPath(crumbScope, tree).filter((group) => group.id !== sole)
  const selectedGroup = tree.groups.get(selected ?? '')
  const selectedNode = byId.get(selected ?? '')
  const selectedLeaves = selectedGroup ? leaves(selectedGroup.id) : []
  const selectedGroupHealth = groupHealth(selectedLeaves, byId, live)
  const groupState = selectedGroup
    ? selectedLeaves.some((id) => !byId.has(id)) && !stateQuery.isError && selectedGroupHealth.kind !== 'run'
      ? PENDING_GRAPH_STATE : selectedGroupHealth
    : undefined
  // How many nodes in view a run is working on now, whoever started it.
  const runningHere = runningCount(scopeIds, byId, live)

  // Against the URL as it is when the change lands, never the render's copy: a navigation is a
  // transition, and a keystroke right after a click (a search typed after going up a level)
  // would otherwise write the click's URL back. react-router's functional form reads the render's
  // params too, so the hash itself is read.
  const update = (changes: ParamChanges) => setParams(applyChanges(currentParams(), changes), { replace: true })
  const select = (id: string | null) => update(selectChanges(graph, id))
  const openGroup = (id: string | null) => update(openGroupChanges(id, sole))
  const revealNode = (id: string) => update(revealChanges(id, tree, graph))

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        select(null)
        return
      }
      if (graph || !selected || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
      if (event.target instanceof Element && event.target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return
      if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
      const next = nextRowIndex(rowIds, selected, event.key)
      if (next === null) return
      event.preventDefault()
      select(rowIds[next]!)
      focusWindowRow(tableBody.current, next, win.scrollToIndex)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  const loading = !assetQuery.data || !groupsQuery.data
  const error = assetQuery.isError || groupsQuery.isError
  // What to do about it, when the server says (e.g. a metadata DB barca cannot use).
  const failure = assetQuery.error ?? groupsQuery.error
  const root = rootName(pipeline)
  const nowMs = stateQuery.dataUpdatedAt
  const readOnly = healthQuery.data?.read_only ?? false

  return (
    <div className="barca-view">
      <StreamNotice notice={nodeRun.stream.notice} runId={nodeRun.run?.handle} />
      <div className="barca-view-head">
        <div className="barca-view-bar">
          {/* The breadcrumb is the title: the page says where it is once. */}
          <nav className="barca-group-crumbs" aria-label="Group breadcrumb">
            {crumbScope && (
              <IconButton label="Up one level" size="sm" onClick={() => openGroup(tree.parents.get(crumbScope) ?? null)}>
                <ArrowLeft size={14} />
              </IconButton>
            )}
            {crumbScope
              ? <button type="button" title={root} onClick={() => openGroup(null)}>{root}</button>
              : <h1 aria-current="page">{root}</h1>}
            {path.map((group, index) => (
              <span key={group.id}>
                <ChevronRight size={12} />
                {index === path.length - 1
                  ? <h1 aria-current="page">{group.name}</h1>
                  : <button type="button" title={group.name} onClick={() => openGroup(group.id)}>{group.name}</button>}
              </span>
            ))}
          </nav>
          <span className="barca-count">
            {plural(scopeIds.length, 'node')}
            {!flat && show === 'items' && ` · ${map ? plural(map.entries.length, 'pipeline') : plural(entries.length, scope ? 'item' : 'group')}`}
          </span>
          {runningHere > 0 && (
            <span className="barca-count barca-count-running">
              <StatusGlyph glyph="running" live size={12} />
              {runningHere} running
            </span>
          )}
          <div className="barca-scope-health" title="Every node in view, by state">
            {stateQuery.data && <HealthBar counts={scopeHealth} labelLimit={3} width={80} />}
          </div>
          {scope && level.external.length > 0 && !flat && !graph && show === 'items' && (
            <span className="barca-group-external" title={level.external.map(shortName).join(', ')}>
              External inputs · {level.external.length}
            </span>
          )}
          <div className="barca-view-actions">
            {nodeRun.run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(nodeRun.run.handle)}`}>View run</Link>}
            <div className="barca-view-toggle" role="group" aria-label="Grouping">
              <button type="button" aria-pressed={!flat && show === 'items'} onClick={() => update(modeChanges('groups', scope))} title="Double-click a group, or press Enter, to open it">
                Groups
              </button>
              <button type="button" aria-pressed={flat} onClick={() => update(modeChanges('flat', scope))}>
                All nodes
              </button>
              <button type="button" aria-pressed={show === 'failures'} onClick={() => update(modeChanges('failures', scope))} data-failed={failureList.length > 0 || undefined}>
                Failures{stateQuery.data ? ` · ${failureList.length}` : ''}
              </button>
            </div>
            {show === 'items' && <ViewToggle />}
            {graph && (
              <>
                <IconButton label={dir === 'LR' ? 'Top-down layout' : 'Left-right layout'} onClick={() => update(dirChanges(dir))}>
                  {dir === 'LR' ? <ArrowDown size={15} /> : <ArrowRight size={15} />}
                </IconButton>
                <IconButton label="Fit to screen" onClick={() => canvas.current?.fit()}>
                  <Maximize size={15} />
                </IconButton>
              </>
            )}
            <PageRunButton run={nodeRun.scopeRun} />
          </div>
        </div>
        {(!graph || show === 'failures') && (
          <div className="barca-table-tools">
            <SearchInput
              placeholder={show === 'failures' ? 'Search failures and errors' : 'Search groups and members'}
              value={view.query}
              onChange={(q) => update({ q: q || null })}
            />
            {show === 'items' && (
              <Select
                label="Sort"
                value={sort}
                options={ENTRY_SORTS.map((value) => ({ value, label: SORT_LABEL[value] }))}
                onChange={(value) => update({ sort: value === 'worst' ? null : value })}
              />
            )}
            <span className="barca-count">{(show === 'failures' ? shownFailures.length : rowCount).toLocaleString()} shown</span>
          </div>
        )}
      </div>

      <RunNotice notice={nodeRun.notice} onDismiss={nodeRun.dismiss} />
      {error ? (
        <div className="barca-table-empty">
          <p>
            Can't load pipeline:{' '}
            {failure?.message}{' '}
            <Button size="sm" onClick={() => { void assetQuery.refetch(); void groupsQuery.refetch() }}>Retry</Button>
          </p>
          {failure instanceof ApiError && failure.remediation && <p>{failure.remediation}</p>}
          {metadataUnusable(failure) && <ResetDatabaseButton readOnly={readOnly} />}
        </div>
      ) : loading ? (
        <p className="barca-table-empty">Loading pipeline…</p>
      ) : (
        <>
        {stateQuery.isError && (
          <div className="barca-graph-tools" role="status">
            Asset state unavailable: {stateQuery.error.message}{' '}
            <Button size="sm" onClick={() => void stateQuery.refetch()}>Retry</Button>
            {stateQuery.error instanceof ApiError && stateQuery.error.remediation && (
              <span> {stateQuery.error.remediation}</span>
            )}
            {/* Starting over mends a database barca cannot use, and nothing else. */}
            {metadataUnusable(stateQuery.error) && <ResetDatabaseButton readOnly={readOnly} />}
          </div>
        )}
        <div className={graph && show === 'items' ? 'barca-graph-wrap' : 'barca-assets-split'}>
          {graph && show === 'items' ? (
            <div className="barca-graph-canvas">
              {tooBig && !lineageOf ? (
                <LineagePicker total={entries.length} nodes={assets} failures={failureList} onPick={select} />
              ) : (
                <>
                  {lineageOf && (
                    <div className="barca-lineage-note" role="status">
                      Lineage of <strong>{shortName(lineageOf)}</strong>: two steps up and down, {drawn.length} of {entries.length.toLocaleString()} nodes.
                      <button type="button" className="barca-clear" onClick={() => select(null)}>Pick another</button>
                    </div>
                  )}
                  <GraphCanvas
                    assets={drawn}
                    dir={dir}
                    selected={selected}
                    onSelect={(id) => (lineageOf && id === null ? undefined : select(id))}
                    onOpenGroup={openGroup}
                    states={visualStates}
                    pins={pins}
                    handleRef={canvas}
                    legend={drawn.some((entry) => entry.kind === 'group') && <span><FolderOpen size={12} />{map ? 'Pipeline' : 'Group'}</span>}
                  />
                </>
              )}
            </div>
          ) : show === 'failures' ? (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <FailuresView
                model={failuresModel}
                rows={failureRows}
                window={win}
                labels={labels}
                placeOf={placeOf}
                selected={selected}
                nowMs={nowMs}
                readOnly={readOnly}
                bodyRef={tableBody}
                onSelect={select}
                dispatch={failuresDispatch}
              />
              {rowCount === 0 && (
                <p className="barca-table-empty">
                  {!stateQuery.data ? 'Checking…' : query ? 'No failures match.' : 'Nothing has failed here.'}
                </p>
              )}
            </div>
          ) : (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <GroupedTable
                entries={visibleEntries}
                tree={tree}
                byId={byId}
                leaves={leaves}
                live={live}
                visualStates={visualStates}
                labels={labels}
                isHit={isHit}
                pathOf={pathOf}
                selected={selected}
                nowMs={nowMs}
                statesLoaded={!!stateQuery.data}
                statesFailed={stateQuery.isError}
                window={win}
                bodyRef={tableBody}
                onSelect={select}
                onOpenGroup={openGroup}
                onReveal={revealNode}
              />
              {visibleEntries.length === 0 && <p className="barca-table-empty">No groups or nodes match.</p>}
            </div>
          )}
          {selectedGroup && groupState && (
            <GroupPanel
              group={selectedGroup}
              tree={tree}
              byId={byId}
              leaves={selectedLeaves}
              live={live}
              state={groupState}
              statesFailed={stateQuery.isError}
              nowMs={nowMs}
              onOpen={openGroup}
              onReveal={revealNode}
              onClose={() => select(null)}
            />
          )}
          {!selectedGroup && selectedNode && (
            <NodePanel
              runAction={nodeRun.action}
              node={selectedNode}
              nodes={states}
              graph={allAssets}
              nowMs={nowMs}
              onSelect={revealNode}
              onOpenGraph={(id) => update({ view: 'graph', group: tree.parents.get(id) ?? null, focus: id, node: null, show: null })}
              onClose={() => select(null)}
            />
          )}
        </div>
        </>
      )}
    </div>
  )
}
