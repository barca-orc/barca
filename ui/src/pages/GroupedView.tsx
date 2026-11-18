import { useState, type RefObject } from 'react'
import { ChevronRight, FolderOpen, X } from 'lucide-react'
import {
  Button,
  Age,
  CauseBadge,
  EstimateCell,
  HealthBar,
  IconButton,
  KeyValue,
  MiddleName,
  RunningBadge,
  SearchInput,
  Section,
  SidePanel,
  StatusBadge,
  StatusGlyph,
  Tag,
  TableSpacer,
} from '@/components'
import { shortName, type GraphEntry } from '@/lib/graph'
import { cacheGraphState, isLive, memberHealth, PENDING_GRAPH_STATE, UNKNOWN_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { blockerOf } from '@/lib/healthDisplay'
import { alsoInNote, groupOutput, groupOutputNote, lastRunDuration, plural, type GroupTree, type NodeGroup } from '@/lib/groups'
import { nodeEstimate, scopeEstimate } from '@/lib/estimates'
import { isFailed, type Failure } from '@/lib/health'
import type { RollupEntry, RollupTree } from '@/lib/rollupView'
import { useTimeZone } from '@/context/timeZone'
import { isCheck, nodeKindLabel } from '@/lib/status'
import type { TableWindow } from '@/hooks/useTableWindow'
import { sourceFile } from '@/lib/pipeline'
import type { NodeState } from '@/lib/types'

/**
 * The grouped page's views: props in, elements out, every decision made by `lib/groupedPage.ts`
 * and the page. Nothing here fetches or reads the URL.
 */

export interface GroupedTableProps {
  entries: readonly GraphEntry[]
  tree: GroupTree
  byId: ReadonlyMap<string, NodeState>
  leaves: (id: string) => string[]
  /** The rollup (`GET /state/rollup`): a group's bar and its failing count are its entry. */
  rollup?: RollupTree
  visualStates: Readonly<Record<string, GraphState>>
  /** What the root picked in the to-do list holds (groups and nodes, by id): the other rows dim. */
  held?: ReadonlySet<string> | null
  /** What each row is called in this table, so no two read the same. */
  labels: ReadonlyMap<string, string>
  /** Rows found by a search beyond this level: clicking one jumps to where it lives. */
  isHit: ReadonlySet<string>
  pathOf: (id: string) => string
  selected: string | null
  /** When the states were fetched: what ages are relative to. */
  nowMs: number
  statesLoaded: boolean
  statesFailed: boolean
  window: TableWindow
  bodyRef: RefObject<HTMLTableSectionElement | null>
  onSelect: (id: string | null) => void
  onOpenGroup: (id: string) => void
  onReveal: (id: string) => void
}

/** Groups and nodes of the open level, worst first: a health bar per group, a state per node. */
/** The failing node that blocks `node`, as the server names it (`root`): one, or none. */
function blockersOf(node: NodeState): string[] {
  const by = blockerOf(node)
  return by ? [by] : []
}

export function GroupedTable({ entries, tree, byId, rollup, visualStates, held = null, labels, isHit, pathOf, selected, nowMs, statesLoaded, statesFailed, window: win, bodyRef, onSelect, onOpenGroup, onReveal }: GroupedTableProps) {
  const hasGroups = entries.some((e) => e.kind === 'group')
  const hasLeaves = entries.some((e) => e.kind !== 'group')
  const columnCount = 3 + (hasGroups ? 3 : 0) + (hasLeaves ? 1 : 0)
  const counts = (id: string) => rollup?.entries.get(id)
  const { zone } = useTimeZone()
  return (
    // The name takes what the fixed columns leave; the low-value columns leave first as the
    // table narrows (shell.css), so it never scrolls sideways at a laptop's width.
    <table className="barca-table barca-group-table">
      <colgroup>
        <col className="gcol-name" /><col className="gcol-state" /><col className="gcol-last" />
        {hasGroups && <col className="gcol-contents" />}
        {hasGroups && <col className="gcol-failed" />}
        {hasLeaves && <col className="gcol-duration" />}
        {hasGroups && <col className="gcol-open" />}
      </colgroup>
      <thead>
        <tr>
          <th className="barca-group-col-name">Name</th>
          <th className="barca-group-col-state" title="Why it is not fresh, in the words and colours of the health map; a group's nodes by cause">State</th>
          <th title="When it is expected to be fresh (an estimate, rounded up), when a tick next rechecks it, or why there is no estimate; a fresh node's last success">Fresh by</th>
          {hasGroups && <th className="gcol-contents">Contents</th>}
          {hasGroups && <th className="num gcol-failed" title="Nodes in the group whose last attempt failed (assets, tasks and sensors)">Failed</th>}
          {hasLeaves && <th className="num gcol-duration" title="The last attempt's">Duration</th>}
          {hasGroups && <th className="barca-group-col-open" />}
        </tr>
      </thead>
      <tbody ref={bodyRef}>
        <TableSpacer height={win.before} columns={columnCount} />
        {win.items.map(({ index }) => {
          const entry = entries[index]
          if (!entry) return null
          const group = tree.groups.get(entry.id)
          const nestedGroups = group?.members.filter((id) => tree.groups.has(id)).length ?? 0
          const state = visualStates[entry.id] ?? UNKNOWN_GRAPH_STATE
          const node = group ? undefined : byId.get(entry.id)
          const running = node?.running?.step === 'running' ? node.running : null
          const duration = group ? null : lastRunDuration(node)
          // The other groups that declare it: the rollup counts it in this one.
          const alsoIn = alsoInNote(rollup?.entries.get(entry.id)?.also_in, tree)
          // When the row is fresh: a group's is its rollup entry's, a node's its own row's.
          // A row with neither loaded yet holds its place with a skeleton.
          const scopeEntry = counts(entry.id)
          const estimate = group ? (scopeEntry ? scopeEstimate(scopeEntry, zone, nowMs) : null) : node ? nodeEstimate(node, zone, nowMs) : null
          return (
            <tr
              key={entry.id}
              ref={win.measure}
              data-index={index}
              tabIndex={0}
              aria-selected={selected === entry.id}
              data-group={group ? entry.id : undefined}
              data-held={held ? held.has(entry.id) : undefined}
              onClick={(event) => {
                event.currentTarget.focus({ preventScroll: true })
                if (isHit.has(entry.id)) onReveal(entry.id)
                else onSelect(selected === entry.id ? null : entry.id)
              }}
              onDoubleClick={() => group && onOpenGroup(entry.id)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') {
                  event.preventDefault()
                  if (group) onOpenGroup(entry.id)
                  else onSelect(entry.id)
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
                      {alsoIn && <span className="barca-cell-sub" title="Declared in these groups too; counted in this one">{alsoIn}</span>}
                    </>
                  )}
                </div>
              </td>
              <td title={running ? undefined : state.hint}>
                {running ? (
                  <RunningBadge running={running} nowMs={nowMs} />
                ) : group && state !== PENDING_GRAPH_STATE && !statesFailed && scopeEntry ? (
                  <HealthBar scope={scopeEntry} />
                ) : node && state.kind === 'cache' ? (
                  <div className="barca-state-line"><CauseBadge cause={node.cause} /></div>
                ) : (
                  <div className="barca-state-line">
                    <StatusBadge glyph={state.glyph} label={state.label} live={isLive(state)} size="sm" />
                  </div>
                )}
              </td>
              <td>
                {estimate && estimate.text === '' && node?.last_success_at
                  ? <span className="barca-cell-sub" title="Its last success">built <Age at={node.last_success_at} nowMs={nowMs} /></span>
                  : <EstimateCell words={estimate} />}
              </td>
              {hasGroups && <td className="gcol-contents">{group && entry.summary}</td>}
              {hasGroups && (
                <td className="num gcol-failed">{group && statesLoaded && counts(entry.id)?.health.failing.toLocaleString()}</td>
              )}
              {hasLeaves && (
                <td className="num gcol-duration" title={duration?.title}>{duration?.text}</td>
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
                        onOpenGroup(entry.id)
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
  )
}

export interface GroupPanelProps {
  group: NodeGroup
  tree: GroupTree
  byId: ReadonlyMap<string, NodeState>
  leaves: string[]
  /** The group's entry of the rollup (`GET /state/rollup`); none until it has loaded. */
  scope: RollupEntry | undefined
  /** The group's box: its most urgent fact, or Checking… while members load. */
  state: GraphState
  statesFailed: boolean
  nowMs: number
  onOpen: (id: string) => void
  onReveal: (id: string) => void
  onClose: () => void
}

/** The selected group's details: its members by state, its failed members, and its output. */
export function GroupPanel({ group, tree, byId, leaves, scope, state, statesFailed, nowMs, onOpen, onReveal, onClose }: GroupPanelProps) {
  // Members whose last attempt failed, by their resting state (a running member is being retried).
  const failedMembers = leaves.map((id) => byId.get(id)).filter((node): node is NodeState => !!node && isFailed(cacheGraphState(node)))
  const outputId = groupOutput(group.id, tree)
  const output = byId.get(outputId)
  // Why the output is not up to date when it did not fail itself: the failing node that blocks
  // it, as the server names it (`root`).
  const outputNote = output && !isFailed(cacheGraphState(output)) ? groupOutputNote(output, blockersOf(output)) : null
  return (
    <SidePanel
      label={`${group.name} group details`}
      title={group.name}
      badge={<span className="barca-sidepanel-kind">group</span>}
      subtitle={sourceFile(group.id.replace(/^group:/, ''))}
      actions={
        <IconButton label="Close group details" size="sm" onClick={onClose}>
          <X size={14} />
        </IconButton>
      }
    >
      <div className="barca-node-summary">
        <StatusBadge glyph={state.glyph} label={state.label} live={isLive(state)} />
        {state !== PENDING_GRAPH_STATE && scope && (
          <div className="barca-panel-health">
            <HealthBar scope={scope} labelLimit={10} width={260} />
          </div>
        )}
        <p className="barca-note">{group.description}</p>
        <Button size="sm" onClick={() => onOpen(group.id)} iconLeft={<FolderOpen size={13} />}>
          Open group
        </Button>
      </div>
      {failedMembers.length > 0 && (
        <Section title="Failed members">
          {failedMembers.map((node) => (
            <button type="button" className="barca-group-failure" key={node.id} onClick={() => onReveal(node.id)}>
              <StatusGlyph glyph="failed" size={12} />
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
        <p className="barca-note">The existing node that represents this group’s result.</p>
        <button type="button" className="barca-group-output-link" onClick={() => onReveal(outputId)}>
          <span>{shortName(outputId)}</span>
          <ChevronRight size={14} />
        </button>
        {output ? (
          <>
            <OutputState node={output} nowMs={nowMs} />
            {outputNote && <p className="barca-note">{outputNote}</p>}
          </>
        ) : (
          <StatusBadge glyph={statesFailed ? 'unknown' : 'pending'} label={statesFailed ? 'State unavailable' : 'Checking…'} size="sm" />
        )}
      </Section>
      <Section title="Members">
        <KeyValue label="assets">{leaves.filter((id) => byId.get(id)?.kind === 'asset').length}</KeyValue>
        <KeyValue label="checks">{leaves.filter((id) => isCheck({ id, kind: byId.get(id)?.kind ?? 'asset' })).length}</KeyValue>
        {group.members.some((id) => tree.groups.has(id)) && (
          <KeyValue label="nested groups">{group.members.filter((id) => tree.groups.has(id)).length}</KeyValue>
        )}
      </Section>
      <Section title="Organization">
        <p className="barca-note">Groups organize existing nodes. Their members keep their own dependencies and caches.</p>
      </Section>
    </SidePanel>
  )
}

/** A node's state as the server says it, with the last attempt as its own fact; or the run on it. */
function OutputState({ node, nowMs }: { node: NodeState; nowMs: number }) {
  if (node.running?.step === 'running') return <RunningBadge running={node.running} nowMs={nowMs} />
  const box = memberHealth(node)
  return (
    <div className="barca-state-line" title={box.hint}>
      <StatusBadge glyph={box.glyph} label={box.label} size="sm" />
    </div>
  )
}

/** In place of a graph too big to draw: pick the node whose lineage to draw. */
export function LineagePicker({ total, nodes, failures, onPick }: {
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
              <li key={f.id}><button type="button" onClick={() => onPick(f.id)}><StatusGlyph glyph="failed" size={12} /> {f.name} <span>{f.headline}</span></button></li>
            ))}
          </ul>
        </>
      )}
    </div>
  )
}
