import { StreamNotice } from '@/components/StreamNotice'
import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
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
  Button,
  ConnectionBadge,
  HealthBar,
  IconButton,
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
} from '@/components'
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
import { connection } from '@/lib/connection'
import { ApiError } from '@/lib/api'
import { formatAgo } from '@/lib/assetTable'
import { inPipeline, pipelineName, sourceFile } from '@/lib/pipeline'
import { runningCount } from '@/lib/running'
import { shortName, type LayoutDir, type GraphEntry } from '@/lib/graph'
import { liveGraphState, PENDING_GRAPH_STATE, UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { consumersOf, failures, healthCounts, type Failure } from '@/lib/health'
import { GRAPH_LIMIT, lineage } from '@/lib/lineage'
import {
  groupTree,
  PREVIEW_FAILURE,
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

/** A database barca cannot use is mended by starting over; any other error is not. */
const metadataUnusable = (error: unknown) => error instanceof ApiError && error.reason === 'metadata_unusable'

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
  const scope = viewScope(params, tree)
  const query = params.get('q') ?? ''
  const hasDemoPreview = (groupsQuery.data ?? []).some(g => g.members.includes(PREVIEW_FAILURE)) && (!pipeline || pipeline === 'modeling.py')
  const preview = hasDemoPreview && params.get('preview') === 'failed'
  const attention = params.get('attention') === '1'
  const assetQuery = useAssets()
  const stateQuery = useAssetStates()
  const healthQuery = useHealth()
  const [dir, setDir] = useState<LayoutDir>('LR')
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
  const tooBig = graph && entries.length > GRAPH_LIMIT
  const lineageOf = tooBig && selected && !tree.groups.has(selected) ? selected : null
  const drawn = useMemo<GraphEntry[]>(
    () => tooBig
      ? lineageOf ? lineage(lineageOf, assets, 2, consumers).map((a) => ({ ...a, name: shortName(a.id) })) : []
      : entries,
    [tooBig, lineageOf, assets, consumers, entries],
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
    const memberIds = leaves(entry.id)
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
    [graph, drawn, entries, hits, byId, leavesOf, stateQuery.isError, nodeRun.statuses],
  )
  // The nodes of the open scope, and what is wrong with them.
  const scopeIds = useMemo(() => (scope ? leaves(scope) : assets.map((a) => a.id)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [scope, leavesOf, assets])
  const scopeHealth = useMemo(() => healthCounts(scopeIds, byId, nodeRun.statuses), [scopeIds, byId, nodeRun.statuses])
  const failureList = useMemo(() => {
    const inScope = new Set(scopeIds)
    return failures(states.filter((s) => inScope.has(s.id)), consumers)
  }, [scopeIds, states, consumers])
  const visibleEntries = [...entries.filter((entry) => {
    const matches =
      !needle ||
      `${entry.name} ${entry.outputName ?? ''} ${leaves(entry.id).join(' ')}`
        .toLowerCase()
        .includes(needle)
    return (
      matches &&
      (!attention ||
        ['failed', 'warning', 'skipped'].includes(
          (visualStates[entry.id] ?? UNKNOWN_GRAPH_STATE).status,
        ))
    )
  }), ...hits.filter((hit) => !attention || ['failed', 'warning', 'skipped'].includes((visualStates[hit.id] ?? UNKNOWN_GRAPH_STATE).status))]
  const isHit = new Set(hits.map((h) => h.id))
  const visibleFailures = needle
    ? failureList.filter((f) => `${f.id} ${f.headline}`.toLowerCase().includes(needle))
    : failureList
  const rowCount = show === 'failures' ? visibleFailures.length : visibleEntries.length
  const rowIds = show === 'failures' ? visibleFailures.map((f) => f.id) : visibleEntries.map((e) => e.id)
  const win = useTableWindow(rowCount, scroller)
  const path = groupPath(scope, tree)
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
  const openGroup = (id: string | null) =>
    update({ group: id, node: null, focus: null, q: null, attention: null, show: null })
  const revealNode = (id: string) =>
    update({
      group: tree.parents.get(id) ?? null,
      node: graph ? null : id,
      focus: graph ? id : null,
      q: null,
      attention: null,
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
      const nextIndex = index + (event.key === 'ArrowDown' ? 1 : -1)
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
  const title = scope ? tree.groups.get(scope)!.name : rootName
  const pathOf = (id: string) => groupPath(tree.parents.get(id) ?? null, tree).map((g) => g.name).join(' › ')

  return (
    <div className="barca-view">
      <StreamNotice notice={nodeRun.stream.notice} runId={nodeRun.run?.handle} />
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
            <span className="barca-count">
              {scopeIds.length.toLocaleString()} nodes
              {!flat && show === 'items' && ` · ${entries.length} ${scope ? 'items' : 'groups'}`}
            </span>
            {runningHere > 0 && (
              <span className="barca-count barca-count-running">
                <StatusDot status="running" size={6} />
                {runningHere} running
              </span>
            )}
          </div>
          <div className="barca-view-actions">
            {nodeRun.run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(nodeRun.run.handle)}`}>View run</Link>}
            <ConnectionBadge
              connection={connection(healthQuery.data, healthQuery.isError)}
            />
            {graph && (
              <>
                <IconButton
                  label={dir === 'LR' ? 'Top-down layout' : 'Left-right layout'}
                  onClick={() => setDir(dir === 'LR' ? 'TB' : 'LR')}
                >
                  {dir === 'LR' ? (
                    <ArrowDown size={15} />
                  ) : (
                    <ArrowRight size={15} />
                  )}
                </IconButton>
                <IconButton
                  label="Fit to screen"
                  onClick={() => canvas.current?.fit()}
                >
                  <Maximize size={15} />
                </IconButton>
              </>
            )}
          </div>
        </div>
        <div className="barca-group-toolbar">
          <nav className="barca-group-crumbs" aria-label="Group breadcrumb">
            {scope && (
              <IconButton
                label="Up one level"
                size="sm"
                onClick={() => openGroup(tree.parents.get(scope) ?? null)}
              >
                <ArrowLeft size={14} />
              </IconButton>
            )}
            <button
              type="button"
              onClick={() => openGroup(null)}
              aria-current={!scope ? 'page' : undefined}
            >
              {rootName}
            </button>
            {path.map((group, index) => (
              <span key={group.id}>
                <ChevronRight size={12} />
                <button
                  type="button"
                  onClick={() => openGroup(group.id)}
                  aria-current={index === path.length - 1 ? 'page' : undefined}
                >
                  {group.name}
                </button>
              </span>
            ))}
          </nav>
          <div className="barca-scope-health" title="Every node in view, by state">
            {stateQuery.data && <HealthBar counts={scopeHealth} labelLimit={4} width={140} />}
          </div>
          {scope && projected.external.length > 0 && !flat && show === 'items' && (
            <span className="barca-group-external" title={projected.external.map(shortName).join(', ')}>
              External inputs · {projected.external.length}
            </span>
          )}
          <div className="barca-group-options">
            <div
              className="barca-view-toggle"
              role="group"
              aria-label="Grouping"
            >
              <button
                type="button"
                aria-pressed={!flat && show === 'items'}
                onClick={() => setMode('groups')}
                title="Double-click a group, or press Enter, to open it"
              >
                Groups
              </button>
              <button
                type="button"
                aria-pressed={flat}
                onClick={() => setMode('flat')}
              >
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
            {hasDemoPreview && <button
              type="button"
              className="barca-preview-toggle"
              aria-pressed={preview}
              onClick={() => update({ preview: preview ? null : 'failed' })}
            >
              <StatusDot status={preview ? 'failed' : 'skipped'} size={6} />
              Preview failed check
            </button>}
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
              <button
                type="button"
                className="barca-preview-toggle"
                aria-pressed={attention}
                onClick={() => update({ attention: attention ? null : '1' })}
              >
                Needs attention
              </button>
            )}
            <span className="barca-count">{rowCount.toLocaleString()} shown</span>
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
                  Group
                </span>
                <span>
                  <StatusDot status="failed" size={6} />
                  Any member failed
                </span>
                <span>
                  <StatusDot status="success" size={6} />
                  Healthy
                </span>
                <span>
                  <StatusDot status="warning" size={6} />
                  Needs refresh
                </span>
                <span>
                  <StatusDot status="running" size={6} />
                  Running
                </span>
              </div>
            </div>
          ) : show === 'failures' ? (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <table className="barca-table barca-group-table barca-failure-table">
                <colgroup>
                  <col style={{ width: '30%' }} />
                  <col style={{ width: '44%' }} />
                  <col style={{ width: '12%' }} />
                  <col style={{ width: '14%' }} />
                </colgroup>
                <thead>
                  <tr>
                    <th>Failed node</th>
                    <th>Error</th>
                    <th>When</th>
                    <th className="num" title="Nodes downstream of it, transitively">Waiting on it</th>
                  </tr>
                </thead>
                <tbody ref={tableBody}>
                  <TableSpacer height={win.before} columns={4} />
                  {win.items.map(({ index }) => {
                    const f = visibleFailures[index]
                    if (!f) return null
                    return (
                      <tr
                        key={f.id}
                        ref={win.measure}
                        data-index={index}
                        tabIndex={0}
                        aria-selected={selected === f.id}
                        onClick={(event) => {
                          event.currentTarget.focus({ preventScroll: true })
                          select(selected === f.id ? null : f.id)
                        }}
                        onKeyDown={(event) => {
                          if (event.key === 'Enter') {
                            event.preventDefault()
                            select(f.id)
                          }
                        }}
                      >
                        <td>
                          <div className="barca-group-row-name">
                            <StatusDot status="failed" size={6} />
                            <strong title={f.id}>{f.name}</strong>
                            {f.kind !== 'asset' && <Tag size="sm">{f.kind === 'task' ? 'check' : f.kind}</Tag>}
                          </div>
                          <div className="barca-cell-sub" title={pathOf(f.id)}>{pathOf(f.id) || sourceFile(f.id)}</div>
                        </td>
                        <td className="barca-failure-error" title={f.error}>
                          {f.headline}
                          {f.missingKeys !== null && f.missingKeys > 0 && (
                            <div className="barca-cell-sub">{f.missingKeys.toLocaleString()} keys without a result</div>
                          )}
                        </td>
                        <td title={f.at ? `${f.at} UTC` : undefined}>{f.at ? formatAgo(f.at, stateQuery.dataUpdatedAt) : '–'}</td>
                        <td className="num">{f.impact > 0 ? f.impact.toLocaleString() : '–'}</td>
                      </tr>
                    )
                  })}
                  <TableSpacer height={win.after} columns={4} />
                </tbody>
              </table>
              {rowCount === 0 && (
                <p className="barca-table-empty">
                  {!stateQuery.data ? 'Checking…' : needle ? 'No failures match.' : 'Nothing has failed here.'}
                </p>
              )}
            </div>
          ) : (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <table className="barca-table barca-group-table">
                <colgroup>
                  <col style={{ width: '34%' }} />
                  <col style={{ width: '30%' }} />
                  <col style={{ width: '16%' }} />
                  <col style={{ width: '16%' }} />
                  <col style={{ width: '4%' }} />
                </colgroup>
                <thead>
                  <tr>
                    <th>Name</th>
                    <th>Health</th>
                    <th>Contents</th>
                    <th>Output</th>
                    <th />
                  </tr>
                </thead>
                <tbody ref={tableBody}>
                  <TableSpacer height={win.before} columns={5} />
                  {win.items.map(({ index }) => {
                    const entry = visibleEntries[index]
                    if (!entry) return null
                    const group = tree.groups.get(entry.id)
                    const nestedGroups = group?.members.filter((id) => tree.groups.has(id)).length ?? 0
                    const state = visualStates[entry.id] ?? UNKNOWN_GRAPH_STATE
                    const outputNode = group
                      ? byId.get(groupOutput(entry.id, tree))
                      : undefined
                    const running = group ? null : byId.get(entry.id)?.running
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
                            <strong>{entry.name}</strong>
                            {group ? (
                              nestedGroups > 0 && <span className="barca-cell-sub">{nestedGroups} groups</span>
                            ) : (
                              <>
                                <Tag size="sm">{entry.kind}</Tag>
                                {isHit.has(entry.id)
                                  ? <span className="barca-cell-sub" title={entry.id}>in {pathOf(entry.id) || sourceFile(entry.id)}</span>
                                  : entry.name !== shortName(entry.id) && <span className="barca-cell-sub" title={entry.id}>{shortName(entry.id)}</span>}
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
                        <td>{group ? entry.summary : '—'}</td>
                        <td>
                          {group && (
                            <div className="barca-group-output-cell">
                              {outputNode && (
                                <StatusDot
                                  status={memberHealth(outputNode).status}
                                  size={6}
                                />
                              )}
                              <span>{entry.outputName}</span>
                            </div>
                          )}
                        </td>
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
                      </tr>
                    )
                  })}
                  <TableSpacer height={win.after} columns={5} />
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
                  {selectedLeaves.filter((id) => byId.get(id)?.kind === 'task').length}
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
