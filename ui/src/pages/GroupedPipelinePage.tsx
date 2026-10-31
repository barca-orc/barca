import { StreamNotice } from '@/components/StreamNotice'
import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useQueryClient } from '@tanstack/react-query'
import { match } from 'ts-pattern'
import {
  ArrowLeft,
  ArrowDown,
  ArrowRight,
  ChevronRight,
  FolderOpen,
  Maximize,
  X,
} from 'lucide-react'
import {
  Age,
  Button,
  HealthBar,
  LastSuccess,
  IconButton,
  MiddleName,
  KeyValue,
  RunningBadge,
  SearchInput,
  Section,
  SidePanel,
  StatusBadge,
  StatusDot,
  Tag,
  TableSpacer,
  ResetDatabaseButton,
  Select,
  ViewToggle,
} from '@/components'
import { isCheck, nodeKindLabel } from '@/lib/status'
import { focusWindowRow, useTableWindow } from '@/hooks/useTableWindow'
import {
  GraphCanvas,
  type GraphCanvasHandle,
} from '@/components/graph/GraphCanvas'
import { NodePanel } from '@/components/assets/NodePanel'
import { useGroups } from '@/hooks/useGroups'
import { useAssets } from '@/hooks/useAssets'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useHealth } from '@/hooks/useHealth'
import { useNodeRun } from '@/hooks/useNodeRun'
import { api, ApiError, metadataUnusable } from '@/lib/api'
import { formatSeconds } from '@/lib/assetTable'
import { INITIAL_FAILURES, MUTED_KEY, failureTable, parseMuted, updateFailures, type FailuresEffect } from '@/lib/failuresView'
import { useProgram } from '@/hooks/useProgram'
import { FailuresView } from './FailuresView'
import { domainFile, domainMap } from '@/lib/domainMap'
import { inPipeline, pipelineName, sourceFile } from '@/lib/pipeline'
import { runningCount } from '@/lib/running'
import { shortName, type LayoutDir, type GraphEntry } from '@/lib/graph'
import { liveGraphState, PENDING_GRAPH_STATE, UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { consumersOf, failures, healthCounts, scopeAge, type Failure } from '@/lib/health'
import { GRAPH_LIMIT, lineage } from '@/lib/lineage'
import {
  compareEntries,
  distinctNames,
  ENTRY_SORTS,
  groupTree,
  parseEntrySort,
  plural,
  soleGroup,
  withPorts,
  PREVIEW_FAILURE,
  type EntryRank,
  type EntrySort,
  groupHealth,
  groupOutput,
  groupPath,
  leavesByGroup,
  memberHealth,
  projectGroups,
  viewScope,
} from '@/lib/groups'
import type { NodeState } from '@/lib/types'
import './groups.css'

/** How many nodes a search lists beyond the open level. */
const SEARCH_LIMIT = 500
/** `x_2` before `x_10`; one collator, not one per comparison. */
const NAME_ORDER = new Intl.Collator(undefined, { numeric: true })
const SORT_LABEL: Record<EntrySort, string> = { worst: 'worst first', name: 'name', oldest: 'oldest success' }

export function GroupedPipelinePage() {
  const [params, setParams] = useSearchParams()
  const pipeline = params.get('pipeline')
  const groupsQuery = useGroups()
  const tree = useMemo(() => groupTree((groupsQuery.data ?? []).filter(g => !pipeline || g.id.startsWith(`group:${pipeline}:`))), [groupsQuery.data, pipeline])
  const leavesOf = useMemo(() => leavesByGroup(tree), [tree])
  const leaves = (id: string) => leavesOf.get(id) ?? [id]
  const graph = params.get('view') === 'graph'
  const flat = params.get('flat') === '1'
  const show = !flat && params.get('show') === 'failures' ? 'failures' : 'items'
  const selected = params.get(graph ? 'focus' : 'node')
  const requestedScope = viewScope(params, tree)
  const query = params.get('q') ?? ''
  const hasDemoPreview = (groupsQuery.data ?? []).some(g => g.members.includes(PREVIEW_FAILURE)) && (!pipeline || pipeline === 'modeling.py')
  const preview = hasDemoPreview && params.get('preview') === 'failed'
  const sort = parseEntrySort(params.get('sort'))
  const assetQuery = useAssets()
  const stateQuery = useAssetStates()
  const healthQuery = useHealth()
  const dir: LayoutDir = params.get('dir') === 'TB' ? 'TB' : 'LR'
  const canvas = useRef<GraphCanvasHandle>(null)
  const tableBody = useRef<HTMLTableSectionElement>(null)
  const scroller = useRef<HTMLDivElement>(null)

  const allAssets = useMemo(() => (assetQuery.isPlaceholderData ? [] : (assetQuery.data ?? [])), [assetQuery.data, assetQuery.isPlaceholderData])
  const assets = useMemo(() => allAssets.filter((a) => inPipeline(a.id, pipeline)), [allAssets, pipeline])
  const byKind = useMemo(() => new Map(assets.map((a) => [a.id, a.kind])), [assets])
  // Lineage crosses pipelines: a consumer in another file is still downstream.
  const consumers = useMemo(() => consumersOf(allAssets), [allAssets])
  const states = useMemo(
    () =>
      (stateQuery.data ?? []).map((node): NodeState => {
        if (!preview || node.id !== PREVIEW_FAILURE) return node
        return {
          ...node,
          last_materialization: {
            status: 'failed',
            created_at:
              node.last_materialization?.created_at ?? '2026-10-09 00:00:00',
            elapsed_seconds: null,
            run_hash: null,
            artifact: null,
            format: null,
            size_bytes: null,
            error:
              'Failure preview: the fold 3 model check found a feature-count mismatch. The model output remains cached.',
          },
        }
      }),
    [stateQuery.data, preview],
  )
  const byId = useMemo(() => new Map(states.map((s) => [s.id, s])), [states])
  const selectedAsset = assets.find(a => a.id === selected) ?? null
  const nodeRun = useNodeRun(selectedAsset)
  // A pipeline that is one group is that group: no card to open first.
  const sole = useMemo(() => soleGroup(assets, tree), [assets, tree])
  const scope = requestedScope ?? (flat ? null : sole)
  const projected = useMemo(
    () => projectGroups(assets, tree, scope),
    [assets, scope, tree],
  )
  const entries = useMemo<GraphEntry[]>(
    () =>
      flat
        ? assets.map((a) => ({ ...a, name: shortName(a.id) }))
        : projected.entries,
    [assets, flat, projected.entries],
  )
  // A graph too big to read is drawn as the selected node's lineage instead.
  const map = useMemo(
    () => (graph && !pipeline && !scope && !flat ? domainMap(allAssets) : null),
    [graph, pipeline, scope, flat, allAssets],
  )
  const tooBig = graph && !map && entries.length > GRAPH_LIMIT
  const lineageOf = tooBig && selected && !tree.groups.has(selected) ? selected : null
  const placeOfInput = (id: string) =>
    groupPath(tree.parents.get(id) ?? null, tree).filter((g) => g.id !== sole).map((g) => g.name).join(' › ') || sourceFile(id)
  const drawn = useMemo<GraphEntry[]>(
    () => map
      ? map.entries
      : tooBig
        ? lineageOf ? lineage(lineageOf, assets, 2, consumers).map((a) => ({ ...a, name: shortName(a.id) })) : []
        : scope && !flat ? withPorts(entries, projected.consumers, placeOfInput) : entries,
    // placeOfInput follows the tree.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [map, tooBig, lineageOf, assets, consumers, entries, scope, flat, projected.consumers, tree],
  )
  // Typing stays ahead of the list: a one-letter search can match thousands of nodes.
  const needle = useDeferredValue(query).trim().toLowerCase()
  // A search reaches every node in scope, not just this level: a match deep in a nested
  // group is listed with its path, and opening it jumps there.
  const hits = useMemo<GraphEntry[]>(() => {
    if (!needle || flat || show === 'failures' || graph) return []
    const level = new Set(entries.map((e) => e.id))
    const ids = scope ? leavesOf.get(scope) ?? [] : assets.map((a) => a.id)
    // What needs attention first, then by name.
    const rank = (id: string) => ({ failed: 0, warning: 1 } as Record<string, number>)[memberHealth(byId.get(id)).status] ?? 2
    return ids
      .filter((id) => !level.has(id) && id.toLowerCase().includes(needle))
      .map((id) => ({ id, rank: rank(id), name: shortName(id) }))
      .sort((a, b) => a.rank - b.rank || NAME_ORDER.compare(a.name, b.name))
      .slice(0, SEARCH_LIMIT)
      .map(({ id }) => id)
      .map((id) => ({ id, kind: byKind.get(id) ?? 'asset', inputs: [], name: shortName(id) }))
    // byKind follows assets.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [needle, flat, show, graph, entries, scope, leavesOf, assets, byId])
  const stateOf = (entry: GraphEntry): GraphState => {
    const memberIds = map?.leaves.get(entry.id) ?? leaves(entry.id)
    const liveStatus = nodeRun.statuses[entry.id]
    const health = entry.kind === 'group'
      ? groupHealth(memberIds, byId, nodeRun.statuses)
      : liveStatus
        ? liveGraphState(liveStatus)
        : memberHealth(byId.get(entry.id))
    const pending = memberIds.some(id => !byId.has(id)) && !stateQuery.isError
    return pending && !['running', 'queued'].includes(health.status) ? PENDING_GRAPH_STATE : health
  }
  const visualStates = useMemo<Record<string, GraphState>>(
    () => Object.fromEntries((graph ? drawn : [...entries, ...hits]).map((entry) => [entry.id, stateOf(entry)])),
    // stateOf reads only what is listed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [graph, drawn, map, entries, hits, byId, leavesOf, stateQuery.isError, nodeRun.statuses],
  )
  // The nodes of the open scope, and what is wrong with them.
  const scopeIds = useMemo(() => (scope ? leaves(scope) : assets.map((a) => a.id)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [scope, leavesOf, assets])
  const scopeHealth = useMemo(() => healthCounts(scopeIds, byId, nodeRun.statuses), [scopeIds, byId, nodeRun.statuses])
  const graphKnown = !assetQuery.isPlaceholderData && !!assetQuery.data
  const failureList = useMemo(() => {
    const inScope = new Set(scopeIds)
    return failures(states.filter((s) => inScope.has(s.id)), graphKnown ? consumers : null)
  }, [scopeIds, states, consumers, graphKnown])
  const rankOf = (entry: GraphEntry): EntryRank => {
    const ids = leaves(entry.id)
    const counts = healthCounts(ids, byId, nodeRun.statuses)
    return { name: entry.name ?? entry.id, failed: counts.failed, stale: counts.stale, lastSuccessAt: scopeAge(ids, byId).lastSuccessAt }
  }
  const sortedEntries = useMemo(() => {
    const ranked = entries.map((entry) => ({ entry, rank: rankOf(entry) }))
    return ranked.sort((a, b) => compareEntries(sort, a.rank, b.rank)).map(({ entry }) => entry)
    // rankOf reads only what is listed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entries, sort, byId, leavesOf, nodeRun.statuses])
  const visibleEntries = [
    ...sortedEntries.filter((entry) => !needle
      || `${entry.name} ${entry.outputName ?? ''} ${leaves(entry.id).join(' ')}`.toLowerCase().includes(needle)),
    ...hits,
  ]
  const isHit = new Set(hits.map((h) => h.id))
  const visibleFailures = needle
    ? failureList.filter((f) => `${f.id} ${f.headline}`.toLowerCase().includes(needle))
    : failureList
  const client = useQueryClient()
  const navigate = useNavigate()
  const [failuresModel, failuresDispatch] = useProgram(
    () => ({ ...INITIAL_FAILURES, muted: parseMuted(localStorage.getItem(MUTED_KEY)) }),
    updateFailures,
    (effect: FailuresEffect, dispatch) => {
      const failed = (error: unknown) => dispatch({ type: 'action_failed', text: error instanceof Error ? error.message : String(error) })
      match(effect)
        .with({ type: 'retry' }, ({ id, kind, name }) => {
          // A task always re-executes; anything else is a cache-aware `get`.
          (kind === 'task' ? api.runTarget(id) : api.getTarget(id)).then(() => {
            void client.invalidateQueries({ queryKey: ['runs'] })
            dispatch({ type: 'done', text: `Retrying ${name}` })
          }, failed)
        })
        .with({ type: 'copy' }, ({ text }) => {
          navigator.clipboard.writeText(text).then(() => dispatch({ type: 'done', text: 'Error copied' }), failed)
        })
        .with({ type: 'open_run' }, ({ id, name }) => {
          api.asset(id).then(({ stats }) => {
            const run = stats.recent_runs.find((r) => r.status !== 'success')?.run_id
            if (run) navigate(`/runs?run=${encodeURIComponent(run)}`)
            else dispatch({ type: 'action_failed', text: `No run is recorded for ${name}` })
          }, failed)
        })
        .with({ type: 'save_muted' }, ({ muted }) => localStorage.setItem(MUTED_KEY, JSON.stringify(muted)))
        .exhaustive()
    },
  )
  const { rows: failureRows, mutedCount } = failureTable(visibleFailures, failuresModel)
  const rowCount = show === 'failures' ? failureRows.length : visibleEntries.length
  // Null for a row that cannot be selected: a cause's heading.
  const rowIds = show === 'failures'
    ? failureRows.map((r) => (r.kind === 'failure' ? r.failure.id : null))
    : visibleEntries.map((e) => e.id)
  const hasGroups = visibleEntries.some((e) => e.kind === 'group')
  const hasLeaves = visibleEntries.some((e) => e.kind !== 'group')
  const columnCount = 3 + (hasGroups ? 3 : 0) + (hasLeaves ? 2 : 0)
  const failedCount = (id: string) => healthCounts(leaves(id), byId, nodeRun.statuses).failed
  // What each row is called in this table, so no two read the same.
  const leafIds = show === 'failures'
    ? visibleFailures.map((f) => f.id)
    : visibleEntries.filter((e) => e.kind !== 'group').map((e) => e.id)
  const labels = distinctNames(leafIds)
  const win = useTableWindow(rowCount, scroller)
  const crumbScope = scope === sole ? null : scope
  const path = groupPath(crumbScope, tree).filter((group) => group.id !== sole)
  const selectedGroup = tree.groups.get(selected ?? '')
  const selectedNode = byId.get(selected ?? '')
  const selectedLeaves = selectedGroup ? leaves(selectedGroup.id) : []
  const selectedFailures = selectedLeaves
    .map((id) => byId.get(id))
    .filter(
      (node): node is NodeState =>
        !!node && memberHealth(node).status === 'failed',
    )
  const output = selectedGroup
    ? byId.get(groupOutput(selectedGroup.id, tree))
    : undefined
  const selectedGroupHealth = groupHealth(selectedLeaves, byId, nodeRun.statuses)
  const groupStatus = selectedGroup
    ? selectedLeaves.some(id => !byId.has(id)) && !stateQuery.isError && selectedGroupHealth.status !== 'running'
      ? PENDING_GRAPH_STATE : selectedGroupHealth
    : undefined
  // How many nodes in view a run is working on now, whoever started it.
  const runningHere = runningCount(scopeIds, byId, nodeRun.statuses)

  const update = (changes: Record<string, string | null>) => {
    const next = new URLSearchParams(params)
    for (const [key, value] of Object.entries(changes)) {
      if (value === null) next.delete(key)
      else next.set(key, value)
    }
    setParams(next, { replace: true })
  }
  const select = (id: string | null) =>
    update({ [graph ? 'focus' : 'node']: id })
  const openGroup = (id: string | null) => {
    const file = id && domainFile(id)
    update({ ...(file ? { pipeline: file, group: null } : { group: id === sole ? null : id }), node: null, focus: null, q: null, show: null })
  }
  const revealNode = (id: string) =>
    update({
      group: tree.parents.get(id) ?? null,
      node: graph ? null : id,
      focus: graph ? id : null,
      q: null,
      show: null,
    })
  const setMode = (mode: 'groups' | 'flat' | 'failures') =>
    update(
      mode === 'flat'
        ? { flat: '1', group: null, node: null, focus: null, q: null, show: null }
        : mode === 'failures'
          ? { flat: null, show: 'failures', view: null, group: scope, node: null, focus: null }
          : { flat: null, show: null, node: null, focus: null },
    )

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        select(null)
        return
      }
      if (
        graph ||
        !selected ||
        event.defaultPrevented ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey
      )
        return
      if (
        event.target instanceof Element &&
        event.target.closest(
          'input, textarea, select, [contenteditable]:not([contenteditable="false"])',
        )
      )
        return
      if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
      const index = rowIds.indexOf(selected)
      if (index < 0) return
      event.preventDefault()
      const step = event.key === 'ArrowDown' ? 1 : -1
      let nextIndex = index + step
      if (rowIds[nextIndex] === null) nextIndex += step
      const next = rowIds[nextIndex]
      if (!next) return
      select(next)
      focusWindowRow(tableBody.current, nextIndex, win.scrollToIndex)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  const loading =
    assetQuery.isPlaceholderData || !assetQuery.data || !groupsQuery.data
  const error = assetQuery.isError || groupsQuery.isError
  // What to do about it, when the server says (e.g. a metadata DB barca cannot use).
  const failure = assetQuery.error ?? groupsQuery.error
  const rootName = pipeline === 'modeling.py' ? 'Modeling' : pipeline ? pipelineName(pipeline) : 'All pipelines'
  const pathOf = (id: string) => groupPath(tree.parents.get(id) ?? null, tree).map((g) => g.name).join(' › ')

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
              ? <button type="button" title={rootName} onClick={() => openGroup(null)}>{rootName}</button>
              : <h1 aria-current="page">{rootName}</h1>}
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
            {scopeIds.length.toLocaleString()} nodes
            {!flat && show === 'items' && ` · ${map ? plural(map.entries.length, 'pipeline') : plural(entries.length, scope ? 'item' : 'group')}`}
          </span>
          {runningHere > 0 && (
            <span className="barca-count barca-count-running">
              <StatusDot status="running" size={6} />
              {runningHere} running
            </span>
          )}
          <div className="barca-scope-health" title="Every node in view, by state">
            {stateQuery.data && <HealthBar counts={scopeHealth} labelLimit={3} width={80} />}
          </div>
          {scope && projected.external.length > 0 && !flat && !graph && show === 'items' && (
            <span className="barca-group-external" title={projected.external.map(shortName).join(', ')}>
              External inputs · {projected.external.length}
            </span>
          )}
          <div className="barca-view-actions">
            {nodeRun.run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(nodeRun.run.handle)}`}>View run</Link>}
            {hasDemoPreview && <button
              type="button"
              className="barca-preview-toggle"
              aria-pressed={preview}
              onClick={() => update({ preview: preview ? null : 'failed' })}
            >
              <StatusDot status={preview ? 'failed' : 'skipped'} size={6} />
              Preview failed check
            </button>}
            <div className="barca-view-toggle" role="group" aria-label="Grouping">
              <button
                type="button"
                aria-pressed={!flat && show === 'items'}
                onClick={() => setMode('groups')}
                title="Double-click a group, or press Enter, to open it"
              >
                Groups
              </button>
              <button type="button" aria-pressed={flat} onClick={() => setMode('flat')}>
                All nodes
              </button>
              <button
                type="button"
                aria-pressed={show === 'failures'}
                onClick={() => setMode('failures')}
                data-failed={failureList.length > 0 || undefined}
              >
                Failures{stateQuery.data ? ` · ${failureList.length}` : ''}
              </button>
            </div>
            {show === 'items' && <ViewToggle />}
            {graph && (
              <>
                <IconButton
                  label={dir === 'LR' ? 'Top-down layout' : 'Left-right layout'}
                  onClick={() => { const next = new URLSearchParams(params); if (dir === 'LR') next.set('dir', 'TB'); else next.delete('dir'); setParams(next, { replace: true }) }}
                >
                  {dir === 'LR' ? <ArrowDown size={15} /> : <ArrowRight size={15} />}
                </IconButton>
                <IconButton label="Fit to screen" onClick={() => canvas.current?.fit()}>
                  <Maximize size={15} />
                </IconButton>
              </>
            )}
          </div>
        </div>
        {preview && (
          <div className="barca-group-context">
            <span className="barca-group-preview-note">
              Preview: fold 3’s model validation fails; its model output stays
              cached.
            </span>
          </div>
        )}
        {(!graph || show === 'failures') && (
          <div className="barca-table-tools">
            <SearchInput
              placeholder={show === 'failures' ? 'Search failures and errors' : 'Search groups and members'}
              value={query}
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
            {show === 'failures' && mutedCount + (failuresModel.showMuted ? 1 : 0) > 0 && (
              <button
                type="button"
                className="barca-preview-toggle"
                aria-pressed={failuresModel.showMuted}
                onClick={() => failuresDispatch({ type: 'toggle_show_muted' })}
              >
                Muted · {mutedCount}
              </button>
            )}
            <span className="barca-count">{(show === 'failures' ? visibleFailures.length - (failuresModel.showMuted ? 0 : mutedCount) : rowCount).toLocaleString()} shown</span>
          </div>
        )}
      </div>

      {nodeRun.action.error && !selectedNode && <p role="alert">Could not start run: {nodeRun.action.error.message}</p>}
      {error ? (
        <div className="barca-table-empty">
          <p>
            Can't load pipeline:{' '}
            {failure?.message}
          </p>
          {failure instanceof ApiError && failure.remediation && <p>{failure.remediation}</p>}
          {metadataUnusable(failure) && <ResetDatabaseButton readOnly={healthQuery.data?.read_only ?? false} />}
        </div>
      ) : loading ? (
        <p className="barca-table-empty">Loading pipeline…</p>
      ) : (
        <>
        {stateQuery.isError && (
          <div className="barca-graph-tools" role="status">
            Asset state unavailable: {stateQuery.error.message}
            {stateQuery.error instanceof ApiError && stateQuery.error.remediation && (
              <span> {stateQuery.error.remediation}</span>
            )}
            {/* Starting over mends a database barca cannot use, and nothing else. */}
            {metadataUnusable(stateQuery.error) && <ResetDatabaseButton readOnly={healthQuery.data?.read_only ?? false} />}
          </div>
        )}
        <div className={graph && show === 'items' ? 'barca-graph-wrap' : 'barca-assets-split'}>
          {graph && show === 'items' ? (
            <div className="barca-graph-canvas">
              {tooBig && !lineageOf ? (
                <LineagePicker
                  total={entries.length}
                  nodes={assets}
                  failures={failureList}
                  onPick={select}
                />
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
                    handleRef={canvas}
                  />
                </>
              )}
              <div className="barca-legend">
                <span>
                  <FolderOpen size={12} />
                  {map ? 'Pipeline' : 'Group'}
                </span>
                <span>
                  <StatusDot status="failed" size={6} />
                  Failed
                </span>
                <span>
                  <StatusDot status="running" size={6} />
                  Running
                </span>
                <span>
                  <StatusDot status="warning" size={6} />
                  Stale
                </span>
                <span>
                  <StatusDot status="skipped" size={6} />
                  Not run
                </span>
                <span>
                  <StatusDot status="success" size={6} />
                  Healthy
                </span>
              </div>
            </div>
          ) : show === 'failures' ? (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <FailuresView
                model={failuresModel}
                rows={failureRows}
                window={win}
                labels={labels}
                placeOf={(id) => pathOf(id) || sourceFile(id)}
                selected={selected}
                nowMs={stateQuery.dataUpdatedAt}
                readOnly={healthQuery.data?.read_only ?? false}
                bodyRef={tableBody}
                onSelect={select}
                dispatch={failuresDispatch}
              />
              {rowCount === 0 && (
                <p className="barca-table-empty">
                  {!stateQuery.data ? 'Checking…' : needle ? 'No failures match.' : 'Nothing has failed here.'}
                </p>
              )}
            </div>
          ) : (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <table className="barca-table barca-group-table">
                <thead>
                  <tr>
                    <th style={{ width: '30%' }}>Name</th>
                    <th style={{ width: '22%' }}>Health</th>
                    <th title="A group shows its oldest member's">Last success</th>
                    {hasGroups && <th>Contents</th>}
                    {hasGroups && <th className="num" title="Failed members">Failures</th>}
                    {hasLeaves && <th title="Since the last attempt">Age</th>}
                    {hasLeaves && <th className="num" title="The last attempt's">Duration</th>}
                    {hasGroups && <th style={{ width: 40 }} />}
                  </tr>
                </thead>
                <tbody ref={tableBody}>
                  <TableSpacer height={win.before} columns={columnCount} />
                  {win.items.map(({ index }) => {
                    const entry = visibleEntries[index]
                    if (!entry) return null
                    const group = tree.groups.get(entry.id)
                    const nestedGroups = group?.members.filter((id) => tree.groups.has(id)).length ?? 0
                    const state = visualStates[entry.id] ?? UNKNOWN_GRAPH_STATE
                    const last = group ? null : byId.get(entry.id)?.last_materialization
                    const running = group ? null : byId.get(entry.id)?.running
                    const { lastSuccessAt, failingSince, failures: failed } = scopeAge(group ? leaves(entry.id) : [entry.id], byId)
                    const age = { at: lastSuccessAt, failingSince, failures: failed }
                    return (
                      <tr
                        key={entry.id}
                        ref={win.measure}
                        data-index={index}
                        tabIndex={0}
                        aria-selected={selected === entry.id}
                        data-group={group ? entry.id : undefined}
                        onClick={(event) => {
                          event.currentTarget.focus({ preventScroll: true })
                          if (isHit.has(entry.id)) revealNode(entry.id)
                          else select(selected === entry.id ? null : entry.id)
                        }}
                        onDoubleClick={() => group && openGroup(entry.id)}
                        onKeyDown={(event) => {
                          if (event.key === 'Enter') {
                            event.preventDefault()
                            if (group) openGroup(entry.id)
                            else select(entry.id)
                          }
                        }}
                      >
                        <td>
                          <div className="barca-group-row-name">
                            {group && <FolderOpen size={15} />}
                            <strong><MiddleName name={labels.get(entry.id) ?? entry.name ?? entry.id} title={entry.id} /></strong>
                            {group ? (
                              nestedGroups > 0 && <span className="barca-cell-sub">{plural(nestedGroups, 'group')}</span>
                            ) : (
                              <>
                                {entry.kind !== 'asset' && entry.kind !== 'group' && entry.kind !== 'port' && <Tag size="sm">{nodeKindLabel({ id: entry.id, kind: entry.kind })}</Tag>}
                                {isHit.has(entry.id) && <span className="barca-cell-sub" title={entry.id}>in {pathOf(entry.id) || sourceFile(entry.id)}</span>}
                              </>
                            )}
                          </div>
                        </td>
                        <td title={running ? undefined : state.hint}>
                          {running ? (
                            <RunningBadge
                              running={running}
                              nowMs={stateQuery.dataUpdatedAt}
                            />
                          ) : group && state !== PENDING_GRAPH_STATE && !stateQuery.isError ? (
                            <HealthBar counts={healthCounts(leaves(entry.id), byId, nodeRun.statuses)} />
                          ) : (
                            <StatusBadge
                              status={state.status}
                              label={state.label}
                              size="sm"
                            />
                          )}
                        </td>
                        <td>
                          {stateQuery.data && <LastSuccess {...age} nowMs={stateQuery.dataUpdatedAt} />}
                        </td>
                        {hasGroups && <td>{group && entry.summary}</td>}
                        {hasGroups && (
                          <td className="num">{group && stateQuery.data && failedCount(entry.id).toLocaleString()}</td>
                        )}
                        {hasLeaves && <td>{last && <Age at={last.created_at} nowMs={stateQuery.dataUpdatedAt} />}</td>}
                        {hasLeaves && (
                          <td className="num">{typeof last?.elapsed_seconds === 'number' ? formatSeconds(last.elapsed_seconds) : '–'}</td>
                        )}
                        {hasGroups && (
                          <td>
                            {group && (
                              <button
                                type="button"
                                className="barca-clear"
                                aria-label={`Open ${group.name}`}
                                onClick={(event) => {
                                  event.stopPropagation()
                                  openGroup(entry.id)
                                }}
                              >
                                <ChevronRight size={15} />
                              </button>
                            )}
                          </td>
                        )}
                      </tr>
                    )
                  })}
                  <TableSpacer height={win.after} columns={columnCount} />
                </tbody>
              </table>
              {visibleEntries.length === 0 && (
                <p className="barca-table-empty">No groups or nodes match.</p>
              )}
            </div>
          )}
          {selectedGroup && groupStatus && (
            <SidePanel
              label={`${selectedGroup.name} group details`}
              title={selectedGroup.name}
              badge={<Tag size="sm">group</Tag>}
              subtitle={sourceFile(selectedGroup.id.replace(/^group:/, ''))}
              actions={
                <IconButton
                  label="Close group details"
                  size="sm"
                  onClick={() => select(null)}
                >
                  <X size={14} />
                </IconButton>
              }
            >
              <div className="barca-node-summary">
                <StatusBadge
                  status={groupStatus.status}
                  label={groupStatus.label}
                />
                {groupStatus !== PENDING_GRAPH_STATE && (
                  <div className="barca-panel-health">
                    <HealthBar counts={healthCounts(selectedLeaves, byId, nodeRun.statuses)} labelLimit={6} width={260} />
                  </div>
                )}
                <p className="barca-note">{selectedGroup.description}</p>
                <Button
                  size="sm"
                  onClick={() => openGroup(selectedGroup.id)}
                  iconLeft={<FolderOpen size={13} />}
                >
                  Open group
                </Button>
              </div>
              {selectedFailures.length > 0 && (
                <Section title="Failed members">
                  {selectedFailures.map((node) => (
                    <button
                      type="button"
                      className="barca-group-failure"
                      key={node.id}
                      onClick={() => revealNode(node.id)}
                    >
                      <StatusDot status="failed" size={6} />
                      <span>
                        {shortName(node.id)}
                        <small>{node.last_materialization?.error?.split('\n')[0]}</small>
                      </span>
                      <ChevronRight size={13} />
                    </button>
                  ))}
                </Section>
              )}
              <Section title="Output">
                <p className="barca-note">
                  The existing node that represents this group’s result.
                </p>
                <button
                  type="button"
                  className="barca-group-output-link"
                  onClick={() =>
                    revealNode(groupOutput(selectedGroup.id, tree))
                  }
                >
                  <span>{shortName(groupOutput(selectedGroup.id, tree))}</span>
                  <ChevronRight size={14} />
                </button>
                {output ? (
                  <StatusBadge
                    status={memberHealth(output).status}
                    label={memberHealth(output).label}
                    size="sm"
                  />
                ) : <StatusBadge status="skipped" label={stateQuery.isError ? 'State unavailable' : 'Checking…'} size="sm" />}
              </Section>
              <Section title="Members">
                <KeyValue label="assets">
                  {selectedLeaves.filter((id) => byId.get(id)?.kind === 'asset').length}
                </KeyValue>
                <KeyValue label="checks">
                  {selectedLeaves.filter((id) => isCheck({ id, kind: byId.get(id)?.kind ?? 'asset' })).length}
                </KeyValue>
                {selectedGroup.members.some((id) => tree.groups.has(id)) && (
                  <KeyValue label="nested groups">
                    {selectedGroup.members.filter((id) => tree.groups.has(id)).length}
                  </KeyValue>
                )}
              </Section>
              <Section title="Organization">
                <p className="barca-note">
                  Groups organize existing nodes. Their members keep their own
                  dependencies and caches.
                </p>
              </Section>
            </SidePanel>
          )}
          {!selectedGroup && selectedNode && (
            <NodePanel
              runAction={nodeRun.action}
              node={selectedNode}
              nodes={states}
              graph={allAssets}
              nowMs={stateQuery.dataUpdatedAt}
              onSelect={revealNode}
              onOpenGraph={(id) =>
                update({
                  view: 'graph',
                  group: tree.parents.get(id) ?? null,
                  focus: id,
                  node: null,
                  show: null,
                })
              }
              onClose={() => select(null)}
            />
          )}
        </div>
        </>
      )}
    </div>
  )
}

/** In place of a graph too big to draw: pick the node whose lineage to draw. */
function LineagePicker({ total, nodes, failures, onPick }: {
  total: number
  nodes: { id: string }[]
  failures: Failure[]
  onPick: (id: string) => void
}) {
  const [q, setQ] = useState('')
  const needle = q.trim().toLowerCase()
  const matches = needle ? nodes.filter((n) => n.id.toLowerCase().includes(needle)).slice(0, 12) : []
  return (
    <div className="barca-lineage-picker">
      <h2>{total.toLocaleString()} nodes are too many to draw as one graph</h2>
      <p className="barca-note">Open a group to see its part of the pipeline, or pick a node to draw its lineage: two steps upstream and downstream.</p>
      <SearchInput placeholder="Find a node" value={q} onChange={setQ} autoFocus />
      <ul>
        {matches.map((n) => (
          <li key={n.id}><button type="button" onClick={() => onPick(n.id)}>{shortName(n.id)} <span>{sourceFile(n.id)}</span></button></li>
        ))}
        {needle && matches.length === 0 && <li className="barca-note">No node matches.</li>}
      </ul>
      {!needle && failures.length > 0 && (
        <>
          <h3>Or start from a failure</h3>
          <ul>
            {failures.slice(0, 6).map((f) => (
              <li key={f.id}><button type="button" onClick={() => onPick(f.id)}><StatusDot status="failed" size={6} /> {f.name} <span>{f.headline}</span></button></li>
            ))}
          </ul>
        </>
      )}
    </div>
  )
}
