import { useEffect, useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router'
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
import { connection } from '@/lib/connection'
import { ApiError } from '@/lib/api'
import { inPipeline } from '@/lib/pipeline'
import { runningCount } from '@/lib/running'
import { shortName, type LayoutDir, type GraphEntry } from '@/lib/graph'
import { PENDING_GRAPH_STATE, UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import {
  groupTree,
  PREVIEW_FAILURE,
  groupHealth,
  groupLeaves,
  groupOutput,
  groupPath,
  memberHealth,
  projectGroups,
} from '@/lib/groups'
import type { NodeState } from '@/lib/types'
import './groups.css'

export function GroupedPipelinePage() {
  const [params, setParams] = useSearchParams()
  const pipeline = params.get('pipeline')
  const groupsQuery = useGroups()
  const tree = useMemo(() => groupTree((groupsQuery.data ?? []).filter(g => !pipeline || g.id.startsWith(`group:${pipeline}:`))), [groupsQuery.data, pipeline])
  const graph = params.get('view') === 'graph'
  const flat = params.get('flat') === '1'
  const selected = params.get(graph ? 'focus' : 'node')
  const requestedScope = tree.groups.has(params.get('group') ?? '')
    ? params.get('group')
    : tree.parents.get(selected ?? '')
  const scope = !flat && requestedScope ? String(requestedScope) : null
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

  const assets = useMemo(
    () =>
      (assetQuery.isPlaceholderData ? [] : (assetQuery.data ?? [])).filter(
        (a) => inPipeline(a.id, pipeline),
      ),
    [assetQuery.data, assetQuery.isPlaceholderData, pipeline],
  )
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
  const visualStates = useMemo<Record<string, GraphState>>(
    () =>
      Object.fromEntries(
        entries.map((entry) => [
          entry.id,
          groupLeaves(entry.id, tree).some(id => !byId.has(id)) && !stateQuery.isError
            ? PENDING_GRAPH_STATE
            : entry.kind === 'group'
              ? groupHealth(groupLeaves(entry.id, tree), byId)
              : memberHealth(byId.get(entry.id)),
        ]),
      ),
    [entries, byId, tree, stateQuery.isError],
  )
  const visible = entries.filter((entry) => {
    const needle = query.trim().toLowerCase()
    const matches =
      !needle ||
      `${entry.name} ${entry.outputName ?? ''} ${groupLeaves(entry.id, tree).join(' ')}`
        .toLowerCase()
        .includes(needle)
    return (
      matches &&
      (!attention ||
        ['failed', 'warning', 'skipped'].includes(
          (visualStates[entry.id] ?? UNKNOWN_GRAPH_STATE).status,
        ))
    )
  })
  const win = useTableWindow(visible.length, scroller)
  // Searching narrows the table; the graph preserves connections and hierarchy.
  const graphEntries = entries
  const path = groupPath(scope, tree)
  const selectedGroup = tree.groups.get(selected ?? '')
  const selectedNode = byId.get(selected ?? '')
  const leaves = selectedGroup ? groupLeaves(selectedGroup.id, tree) : []
  const failures = leaves
    .map((id) => byId.get(id))
    .filter(
      (node): node is NodeState =>
        !!node && memberHealth(node).status === 'failed',
    )
  const output = selectedGroup
    ? byId.get(groupOutput(selectedGroup.id, tree))
    : undefined
  const groupStatus = selectedGroup
    ? leaves.some(id => !byId.has(id)) && !stateQuery.isError
      ? PENDING_GRAPH_STATE : groupHealth(leaves, byId)
    : undefined
  // How many nodes in view a run is working on now, whoever started it.
  const runningHere = runningCount(
    scope ? groupLeaves(scope, tree) : assets.map((a) => a.id),
    byId,
  )

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
    update({ group: id, node: null, focus: null, q: null, attention: null })
  const revealNode = (id: string) =>
    update({
      group: tree.parents.get(id) ?? null,
      node: graph ? null : id,
      focus: graph ? id : null,
      q: null,
      attention: null,
    })

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
      const index = visible.findIndex((entry) => entry.id === selected)
      if (index < 0) return
      event.preventDefault()
      const nextIndex = index + (event.key === 'ArrowDown' ? 1 : -1)
      const next = visible[nextIndex]
      if (!next) return
      select(next.id)
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
  const title = scope
    ? tree.groups.get(scope)!.name
    : pipeline === 'modeling.py'
      ? 'Modeling'
      : 'All pipelines'

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
            <span className="barca-count">
              {entries.length} {flat ? 'nodes' : 'items'} ·{' '}
              {scope ? groupLeaves(scope, tree).length : assets.length} total
              nodes
            </span>
            {runningHere > 0 && (
              <span className="barca-count barca-count-running">
                <StatusDot status="running" size={6} />
                {runningHere} running
              </span>
            )}
          </div>
          <div className="barca-view-actions">
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
              {pipeline === 'modeling.py' ? 'Modeling' : 'All pipelines'}
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
          <div className="barca-group-options">
            <div
              className="barca-view-toggle"
              role="group"
              aria-label="Grouping"
            >
              <button
                type="button"
                aria-pressed={!flat}
                onClick={() => update({ flat: null, node: null, focus: null })}
              >
                Groups
              </button>
              <button
                type="button"
                aria-pressed={flat}
                onClick={() =>
                  update({
                    flat: '1',
                    group: null,
                    node: null,
                    focus: null,
                    q: null,
                  })
                }
              >
                All nodes
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
        <div className="barca-group-context">
          {preview ? (
            <span className="barca-group-preview-note">
              Preview: fold 3’s model validation fails; its model output stays
              cached.
            </span>
          ) : (
            <span>Double-click a group to open it. Enter works too.</span>
          )}
          {scope && projected.external.length > 0 && (
            <span title={projected.external.map(shortName).join(', ')}>
              External inputs · {projected.external.length}
            </span>
          )}
        </div>
        {!graph && (
          <div className="barca-table-tools">
            <SearchInput
              placeholder="Search groups and members"
              value={query}
              onChange={(q) => update({ q: q || null })}
            />
            <button
              type="button"
              className="barca-preview-toggle"
              aria-pressed={attention}
              onClick={() => update({ attention: attention ? null : '1' })}
            >
              Needs attention
            </button>
            <span className="barca-count">{visible.length} shown</span>
          </div>
        )}
      </div>

      {error ? (
        <div className="barca-table-empty">
          <p>
            Can't load pipeline:{' '}
            {failure?.message}
          </p>
          {failure instanceof ApiError && failure.remediation && <p>{failure.remediation}</p>}
          {/* A metadata database that cannot be read or migrated is mended by starting over. */}
          <ResetDatabaseButton readOnly={healthQuery.data?.read_only ?? false} />
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
            <ResetDatabaseButton readOnly={healthQuery.data?.read_only ?? false} />
          </div>
        )}
        <div className={graph ? 'barca-graph-wrap' : 'barca-assets-split'}>
          {graph ? (
            <div className="barca-graph-canvas">
              <GraphCanvas
                assets={graphEntries}
                dir={dir}
                selected={selected}
                onSelect={select}
                onOpenGroup={openGroup}
                states={visualStates}
                handleRef={canvas}
              />
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
          ) : (
            <div className="barca-view-body barca-table-scroll" ref={scroller}>
              <table className="barca-table barca-group-table">
                <colgroup>
                  <col style={{ width: '34%' }} />
                  <col style={{ width: '24%' }} />
                  <col style={{ width: '16%' }} />
                  <col style={{ width: '22%' }} />
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
                    const entry = visible[index]
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
                          select(selected === entry.id ? null : entry.id)
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
                            <Tag size="sm">{entry.kind}</Tag>
                          </div>
                          {(!group || nestedGroups > 0) && (
                            <div className="barca-cell-sub">
                              {group ? `${nestedGroups} nested groups` : shortName(entry.id)}
                            </div>
                          )}
                        </td>
                        <td title={running ? undefined : state.hint}>
                          {running ? (
                            <RunningBadge
                              running={running}
                              nowMs={stateQuery.dataUpdatedAt}
                            />
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
                              <span>{entry.outputName}</span>
                              {outputNode && (
                                <StatusDot
                                  status={memberHealth(outputNode).status}
                                  size={6}
                                />
                              )}
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
              {visible.length === 0 && (
                <p className="barca-table-empty">No groups or nodes match.</p>
              )}
            </div>
          )}
          {selectedGroup && groupStatus && (
            <SidePanel
              label={`${selectedGroup.name} group details`}
              title={selectedGroup.name}
              badge={<Tag size="sm">group</Tag>}
              subtitle="modeling.py"
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
                <p className="barca-note">{selectedGroup.description}</p>
                <Button
                  size="sm"
                  onClick={() => openGroup(selectedGroup.id)}
                  iconLeft={<FolderOpen size={13} />}
                >
                  Open group
                </Button>
              </div>
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
                  {leaves.filter((id) => byId.get(id)?.kind === 'asset').length}
                </KeyValue>
                <KeyValue label="checks">
                  {leaves.filter((id) => byId.get(id)?.kind === 'task').length}
                </KeyValue>
                {selectedGroup.members.some((id) => tree.groups.has(id)) && (
                  <KeyValue label="nested groups">
                    {selectedGroup.members.filter((id) => tree.groups.has(id)).length}
                  </KeyValue>
                )}
              </Section>
              {failures.length > 0 && (
                <Section title="Failed members">
                  {failures.map((node) => (
                    <button
                      type="button"
                      className="barca-group-failure"
                      key={node.id}
                      onClick={() => revealNode(node.id)}
                    >
                      <StatusDot status="failed" size={6} />
                      <span>
                        {shortName(node.id)}
                        <small>{node.last_materialization?.error}</small>
                      </span>
                      <ChevronRight size={13} />
                    </button>
                  ))}
                </Section>
              )}
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
              node={selectedNode}
              nodes={states}
              nowMs={stateQuery.dataUpdatedAt}
              onSelect={revealNode}
              onOpenGraph={(id) =>
                update({
                  view: 'graph',
                  group: tree.parents.get(id) ?? null,
                  focus: id,
                  node: null,
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
