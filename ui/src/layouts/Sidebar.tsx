import { Link, useLocation, useNavigate, useSearchParams } from 'react-router'
import { useMemo, type CSSProperties, type ReactNode } from 'react'
import { Check, ChevronDown, ChevronRight, Layers, ListOrdered, PanelLeft, Plus } from 'lucide-react'
import { IconButton, StatusDot } from '@/components'
import { useAssetStates, useWholeState } from '@/hooks/useAssetStates'
import { useAssets } from '@/hooks/useAssets'
import { useGroups } from '@/hooks/useGroups'
import { useChains } from '@/hooks/useChains'
import { SCRATCH } from '@/lib/chains'
import { useProgram } from '@/hooks/useProgram'
import { severityStatus } from '@/lib/assetTable'
import { groupHealth, groupTree, leavesByGroup } from '@/lib/groups'
import { severityWord } from '@/lib/status'
import { mergeStates, pipelineSummaries, type PipelineSummary } from '@/lib/pipeline'
import { SIDEBAR_KEY, fileGroups, updateSidebar, type SidebarGroup, type SidebarModel, type SidebarMsg } from '@/lib/sidebar'
import type { Dispatch } from '@/lib/program'
import { authMode, connect } from '@/lib/credentials'
import markUrl from '@/assets/brand/mark.svg'

/** Pages that can show one pipeline; elsewhere a pipeline click opens Assets. */
const FILTERABLE = ['/assets', '/graph', '/tasks', '/sensors', '/schedules']

export interface SidebarViewProps {
  model: SidebarModel
  /** Null while the graph loads. */
  total: number | null
  pipelines: readonly PipelineSummary[]
  /** The tree under a file. */
  groupsOf: (file: string) => SidebarGroup[]
  selected: string | null
  /** The open group. */
  scope: string | null
  onSelect: (file: string | null) => void
  onOpenGroup: (file: string, group: string) => void
  dispatch: Dispatch<SidebarMsg>
  footer?: ReactNode
  chains?: ReactNode
}

/** The pipelines and, under an open one, its groups, for any model and server data. */
export function SidebarView({ model, total, pipelines, groupsOf, selected, scope, onSelect, onOpenGroup, dispatch, footer, chains }: SidebarViewProps) {
  return (
    <aside className="barca-sidebar" data-collapsed={model.collapsed || undefined}>
      <div className="barca-brand">
        <img src={markUrl} width="22" height="22" alt="" />
        <span className="barca-wordmark">barca</span>
        <span className="barca-side-toggle">
          <IconButton size="sm" label={model.collapsed ? 'Expand sidebar' : 'Collapse sidebar'} onClick={() => dispatch({ type: 'toggle_collapsed' })}>
            <PanelLeft size={14} />
          </IconButton>
        </span>
      </div>

      {chains}
      <div className="barca-sect">
        <span>{chains ? 'Files' : 'Pipelines'}</span>
      </div>
      <div className="barca-pipes">
        {total === null && <span className="barca-pipe-empty">loading…</span>}
        {total !== null && pipelines.length === 0 && (
          <span className="barca-pipe-empty">no pipelines served</span>
        )}
        {pipelines.length > 1 && (
          <button
            className="barca-pipe"
            title="All pipelines"
            data-active={selected === null ? '' : undefined}
            onClick={() => onSelect(null)}
          >
            <Layers size={12} />
            <span className="barca-pipe-name">All pipelines</span>
            <span className="barca-pipe-count">{total}</span>
          </button>
        )}
        {pipelines.map((p) => {
          const groups = model.collapsed ? [] : groupsOf(p.file)
          const forced = selected === p.file || pipelines.length === 1
          const open = forced || model.expanded.includes(p.file)
          return (
            <div key={p.file}>
              <div className="barca-pipe-row">
                <button
                  type="button"
                  className="barca-pipe-chevron"
                  aria-label={`${open ? 'Close' : 'Open'} ${p.name}`}
                  aria-expanded={open}
                  disabled={groups.length === 0 || forced}
                  onClick={() => dispatch({ type: 'toggle_file', file: p.file })}
                >
                  {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                </button>
                <button
                  className="barca-pipe"
                  title={p.known ? `${p.file} · ${severityWord(p.worst)}` : p.file}
                  data-active={selected === p.file ? '' : undefined}
                  aria-pressed={selected === p.file}
                  onClick={() => onSelect(selected === p.file ? null : p.file)}
                >
                  {/* A dot says the whole file's state: none until all of it is known. */}
                  {p.running || p.known
                    ? <StatusDot status={p.running ? 'running' : severityStatus(p.worst)} size={6} />
                    : <span className="barca-pipe-nodot" />}
                  <span className="barca-pipe-name">{p.name}</span>
                  {p.running > 0 && (
                    <span className="barca-pipe-running" title={`${p.running} running now`}>
                      {p.running} running
                    </span>
                  )}
                  <span className="barca-pipe-count">{p.total}</span>
                </button>
              </div>
              {open && groups.map((g) => (
                <button
                  key={g.id}
                  type="button"
                  className="barca-pipe-group"
                  style={{ '--depth': g.depth } as CSSProperties}
                  aria-current={scope === g.id ? 'true' : undefined}
                  title={g.name}
                  onClick={() => onOpenGroup(p.file, g.id)}
                >
                  {g.status ? <StatusDot status={g.status} size={6} /> : <span className="barca-pipe-nodot" />}
                  <span className="barca-pipe-name">{g.name}</span>
                </button>
              ))}
            </div>
          )
        })}
      </div>
      <div className="barca-side-foot">{footer}</div>
    </aside>
  )
}

export function Sidebar() {
  const { data: scoped } = useAssetStates()
  const { data: whole } = useWholeState()
  const assetQuery = useAssets()
  const assets = assetQuery.isPlaceholderData ? undefined : assetQuery.data
  // The open scope is fresh; the pipelines outside it keep what the whole project last said.
  const nodes = useMemo(() => mergeStates(whole, scoped), [whole, scoped])
  const pipelines = useMemo(() => pipelineSummaries(nodes, assets ?? []), [nodes, assets])
  const groups = useGroups()
  const tree = useMemo(() => groupTree(groups.data ?? []), [groups.data])
  const leavesOf = useMemo(() => leavesByGroup(tree), [tree])
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes])
  const location = useLocation()
  const navigate = useNavigate()
  const [params] = useSearchParams()
  const [model, dispatch] = useProgram<SidebarModel, SidebarMsg, { type: 'save_collapsed'; collapsed: boolean }>(
    () => ({ collapsed: localStorage.getItem(SIDEBAR_KEY) === '1', expanded: [] }),
    updateSidebar,
    ({ collapsed }) => localStorage.setItem(SIDEBAR_KEY, collapsed ? '1' : '0'),
  )
  // The selected pipeline lives in the URL (?pipeline=<file>) — the one source
  // the Assets table and the graph both read.
  const selected = params.get('pipeline')
  const scope = params.get('group')

  const select = (file: string | null) => {
    const next = new URLSearchParams(params)
    if (file === null) next.delete('pipeline')
    else next.set('pipeline', file)
    next.delete('node')
    next.delete('focus')
    next.delete('group')
    next.delete('q')
    const path = FILTERABLE.includes(location.pathname) ? location.pathname : '/assets'
    const search = next.toString()
    navigate({ pathname: path, search: search ? `?${search}` : '' })
  }
  const chainParam = params.get('chain')
  const chains = useChains()
  const openChains = (chainParam ?? '').split(',').filter(Boolean)
  /** Click opens one chain; Cmd/Ctrl/Shift-click adds or removes it from the open set. */
  const openChain = (id: string, add = false) => {
    const ids = add ? (openChains.includes(id) ? openChains.filter((c) => c !== id) : [...openChains, id]) : [id]
    if (ids.length === 0) { select(null); return }
    const next = new URLSearchParams({ chain: ids.join(',') })
    const nodes = params.get('nodes')
    if (nodes && ids.includes(SCRATCH)) next.set('nodes', nodes)
    if (params.get('view') === 'graph') next.set('view', 'graph')
    navigate({ pathname: '/assets', search: `?${next}` })
  }
  const chainSection = chains.length > 0 ? (
    <>
      <div className="barca-sect">
        <button type="button" className="barca-sect-link" aria-current={params.get('chains') === 'all' ? 'page' : undefined} onClick={() => navigate('/assets?chains=all')}>Chains</button>
        <button type="button" className="barca-pipe-chevron" title="New scratch chain" aria-label="New scratch chain" onClick={() => openChain(SCRATCH)}>
          <Plus size={12} />
        </button>
      </div>
      <div className="barca-pipes">
        {chains.map(({ def, members }) => {
          // The same rule as a group's health: any failed member makes the chain red.
          const health = groupHealth(members, byId)
          return (
            <div key={def.id} className="barca-pipe-row barca-chain-row" data-multi={openChains.length > 0 || undefined}>
            <button
              type="button"
              className="barca-chain-check"
              role="checkbox"
              aria-checked={openChains.includes(def.id)}
              aria-label={`View ${def.name} together with the open chains`}
              title="View with the open chains"
              onClick={() => openChain(def.id, true)}
            >
              {openChains.includes(def.id) && <Check size={10} strokeWidth={3} />}
            </button>
            <button
              className="barca-pipe"
              title={`${def.description ?? def.name} · ⌘/Shift-click to view with other chains`}
              data-active={openChains.includes(def.id) ? '' : undefined}
              aria-pressed={openChains.includes(def.id)}
              onClick={(e) => openChain(def.id, e.metaKey || e.ctrlKey || e.shiftKey)}
            >
              {members.every((m) => byId.has(m))
                ? <StatusDot status={health.status} size={6} />
                : <span className="barca-pipe-nodot" />}
              <span className="barca-pipe-name">{def.name}</span>
              {health.status === 'running' && <span className="barca-pipe-running">{health.label}</span>}
              <span className="barca-pipe-count">{members.length}</span>
            </button>
            </div>
          )
        })}
        {openChains.includes(SCRATCH) && (
          <div className="barca-pipe-row barca-chain-row" data-multi>
            <button type="button" className="barca-chain-check" role="checkbox" aria-checked aria-label="Close the scratch chain" onClick={() => openChain(SCRATCH, true)}>
              <Check size={10} strokeWidth={3} />
            </button>
            <button className="barca-pipe" data-active="" aria-pressed>
              <ListOrdered size={12} />
              <span className="barca-pipe-name">Scratch chain</span>
            </button>
          </div>
        )}
      </div>
    </>
  ) : undefined
  const openGroup = (file: string, group: string) => {
    const next = new URLSearchParams({ pipeline: file, group })
    navigate({ pathname: '/assets', search: `?${next}` })
  }

  return (
    <SidebarView
      model={model}
      total={assets ? assets.length : null}
      pipelines={pipelines}
      groupsOf={(file) => fileGroups(file, tree, scope, leavesOf, byId)}
      selected={chainParam || params.get('chains') ? '__chain__' : selected}
      scope={scope}
      onSelect={select}
      onOpenGroup={openGroup}
      dispatch={dispatch}
      chains={chainSection}
      footer={<>
        <Link className="barca-nav" to="/docs">Docs</Link>
        {authMode() === 'token' && <><Link className="barca-nav" to="/tokens">API tokens</Link><button className="barca-nav" onClick={() => connect(null)}>Disconnect</button></>}
      </>}
    />
  )
}
