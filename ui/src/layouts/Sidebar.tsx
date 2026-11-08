import { NavLink, useLocation, useNavigate, useSearchParams } from 'react-router'
import { useMemo, type CSSProperties, type ReactNode } from 'react'
import { BookOpen, ChevronDown, ChevronRight, KeyRound, Layers, LogOut, Moon, PanelLeft, Search, Sun } from 'lucide-react'
import { Button, ConnectionBadge, IconButton, RunsInProgress, StatusGlyph } from '@/components'
import { useAssetStates, useWholeState } from '@/hooks/useAssetStates'
import { useAssets } from '@/hooks/useAssets'
import { useGroups } from '@/hooks/useGroups'
import { useHealth } from '@/hooks/useHealth'
import { usePinNotes } from '@/hooks/usePins'
import { useProgram } from '@/hooks/useProgram'
import { useTheme } from '@/context/theme'
import { useTimeZone } from '@/context/timeZone'
import { buildRows } from '@/lib/assetTable'
import { connection } from '@/lib/connection'
import { groupTree, leavesByGroup } from '@/lib/groups'
import { blockedBy } from '@/lib/graphView'
import { inPipeline, mergeStates, pipelineSummaries, type PipelineSummary } from '@/lib/pipeline'
import { plural } from '@/lib/plural'
import { SIDEBAR_KEY, fileGroups, updateSidebar, type SidebarGroup, type SidebarModel, type SidebarMsg } from '@/lib/sidebar'
import { zoneName } from '@/lib/time'
import type { Dispatch } from '@/lib/program'
import type { NodeKind } from '@/lib/types'
import { authMode, connect } from '@/lib/credentials'
import markUrl from '@/assets/brand/mark.svg'
import { NAV_ITEMS, type NavId } from './nav'

/** Pages that can show one pipeline; elsewhere a pipeline click opens Assets. */
const FILTERABLE = ['/assets', '/graph', '/tasks', '/sensors', '/schedules']

/** What the pipelines' counts and glyphs are of on each page: the page's own kind, else assets. */
function countedKind(pathname: string): NodeKind {
  return pathname === '/tasks' ? 'task' : pathname === '/sensors' ? 'sensor' : 'asset'
}

/** The plural word of a kind, for the sidebar's count label. */
const KIND_WORD: Record<NodeKind, string> = { asset: 'asset', task: 'task', sensor: 'sensor' }

export interface SidebarViewProps {
  model: SidebarModel
  /** Nodes of the counted kind in the whole project; null while the graph loads. */
  total: number | null
  /** What the counts are of: `asset`, `task` or `sensor`. */
  counted?: NodeKind
  pipelines: readonly PipelineSummary[]
  /** The tree under a file. */
  groupsOf: (file: string) => SidebarGroup[]
  selected: string | null
  /** The open page shows one pipeline at a time: the selected one is marked. */
  filtering?: boolean
  /** The open group. */
  scope: string | null
  onSelect: (file: string | null) => void
  onOpenGroup: (file: string, group: string) => void
  dispatch: Dispatch<SidebarMsg>
  /** Under the brand: the search button. */
  search?: ReactNode
  /** The main navigation, above the pipelines. */
  nav?: ReactNode
  footer?: ReactNode
}

/**
 * The app's one navigation: the brand, search, the sections (Assets, Runs, Schedules, Tasks,
 * Sensors), then the pipelines (source files) that filter them, and the settings at the foot.
 * Collapsed, every line keeps its icon or glyph and its name in a tooltip.
 */
export function SidebarView({ model, total, counted = 'asset', pipelines, groupsOf, selected, filtering = true, scope, onSelect, onOpenGroup, dispatch, search, nav, footer }: SidebarViewProps) {
  const word = KIND_WORD[counted]
  return (
    <aside className="barca-sidebar" aria-label="Sidebar" data-collapsed={model.collapsed || undefined}>
      <div className="barca-brand">
        <img src={markUrl} width="20" height="20" alt="" />
        <span className="barca-wordmark">barca</span>
        <span className="barca-side-toggle">
          <IconButton size="sm" label={model.collapsed ? 'Expand sidebar' : 'Collapse sidebar'} onClick={() => dispatch({ type: 'toggle_collapsed' })}>
            <PanelLeft size={14} />
          </IconButton>
        </span>
      </div>
      {search}
      {nav}

      <div className="barca-sect">
        <span>Pipelines</span>
        {/* What the numbers beside each file count: the open page's kind. */}
        <span className="barca-sect-note">{word}s</span>
      </div>
      <div className="barca-pipes">
        {total === null && <span className="barca-pipe-empty">loading…</span>}
        {total !== null && pipelines.length === 0 && (
          <span className="barca-pipe-empty">no pipelines served</span>
        )}
        {pipelines.length > 1 && (
          <div className="barca-pipe-row">
            <span className="barca-pipe-chevron" aria-hidden="true" />
            <button
              className="barca-pipe"
              title={`All pipelines · ${plural(total ?? 0, word)}`}
              onClick={() => onSelect(null)}
            >
              <Layers size={13} className="barca-pipe-icon" />
              <span className="barca-pipe-name">All pipelines</span>
              <span className="barca-pipe-count">{total}</span>
            </button>
          </div>
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
                  title={`${p.file} · ${plural(p.total, word)}${p.shown ? ` · ${p.shown.label}` : ''}`}
                  data-active={filtering && selected === p.file ? '' : undefined}
                  aria-pressed={selected === p.file}
                  onClick={() => onSelect(selected === p.file ? null : p.file)}
                >
                  {/* The file's one state, as its header in the table shows it: none until all of it is known. */}
                  {p.shown
                    ? <StatusGlyph glyph={p.shown.glyph} live={p.shown.live} size={12} />
                    : <span className="barca-pipe-nodot" />}
                  <span className="barca-pipe-name">{p.name}</span>
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
                  {g.glyph ? <StatusGlyph glyph={g.glyph} live={g.glyph === 'running'} size={12} /> : <span className="barca-pipe-nodot" />}
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

export interface SidebarNavProps {
  /** How many of each section's items the open pipeline (or the project) has; null while loading. */
  counts: Partial<Record<NavId, number | null>>
  /** Kept on every link: the open pipeline. */
  search: string
  /** After a section's name: what is live in it (runs in progress). */
  extras?: Partial<Record<NavId, ReactNode>>
}

/** The sections, each an icon, its name and a muted count; the open one on a neutral fill. */
export function SidebarNav({ counts, search, extras = {} }: SidebarNavProps) {
  return (
    <nav className="barca-side-nav" aria-label="Main navigation">
      {NAV_ITEMS.map((item) => {
        const count = counts[item.id]
        return (
          <NavLink key={item.id} to={{ pathname: item.path, search }} className="barca-nav" aria-label={item.label}
            title={count == null ? item.label : `${item.label} · ${count.toLocaleString()}`}>
            <item.icon size={15} />
            <span className="barca-nav-label">{item.label}</span>
            {extras[item.id]}
            {count != null && <span className="barca-nav-count">{count.toLocaleString()}</span>}
          </NavLink>
        )
      })}
    </nav>
  )
}

/** The search button under the brand: opens the command palette (⌘K). */
export function SidebarSearch({ onOpen }: { onOpen: () => void }) {
  return (
    <button type="button" className="barca-side-search" aria-label="Search (⌘K)" title="Search assets, runs and actions (⌘K)" onClick={onOpen}>
      <Search size={14} />
      <span className="barca-side-search-text">Search</span>
      <kbd>⌘K</kbd>
    </button>
  )
}

/** The foot: the docs, the connection, the time zone and the theme (and, signed in with a token, its pages). */
function SidebarFooter() {
  const { theme, toggleTheme } = useTheme()
  const { zone, toggleZone } = useTimeZone()
  const health = useHealth()
  const token = authMode() === 'token'
  return (
    <>
      <div className="barca-side-links">
        <a className="barca-nav" href="https://barca.sh/" target="_blank" rel="noreferrer noopener" aria-label="Docs" title="Docs (barca.sh)">
          <BookOpen size={15} /><span className="barca-nav-label">Docs</span>
        </a>
        {token && <>
          <NavLink className="barca-nav" to="/tokens" aria-label="API tokens" title="API tokens">
            <KeyRound size={15} /><span className="barca-nav-label">API tokens</span>
          </NavLink>
          <button type="button" className="barca-nav" aria-label="Disconnect" title="Disconnect" onClick={() => connect(null)}>
            <LogOut size={15} /><span className="barca-nav-label">Disconnect</span>
          </button>
        </>}
      </div>
      <div className="barca-side-settings">
        <ConnectionBadge connection={connection(health.data, health.isError)} />
        <Button
          variant="ghost"
          size="sm"
          className="barca-zone"
          aria-pressed={zone === 'utc'}
          title={zone === 'utc' ? `Times are UTC; show them in local time (${zoneName('local')})` : `Times are local (${zoneName('local')}); show them in UTC`}
          onClick={toggleZone}
        >
          {zoneName(zone)}
        </Button>
        <IconButton size="sm" label={theme === 'light' ? 'Switch to dark' : 'Switch to light'} onClick={toggleTheme}>
          {theme === 'light' ? <Moon size={14} /> : <Sun size={14} />}
        </IconButton>
      </div>
    </>
  )
}

export function Sidebar({ onSearch }: { onSearch: () => void }) {
  const { data: scoped } = useAssetStates()
  const { data: whole } = useWholeState()
  const assetQuery = useAssets()
  const assets = assetQuery.data
  const pins = usePinNotes()
  const location = useLocation()
  const kind = countedKind(location.pathname)
  // The open scope is fresh; the pipelines outside it keep what the whole project last said.
  const nodes = useMemo(() => mergeStates(whole, scoped), [whole, scoped])
  // The rows the tables show, built the same way, so a file's line here and its header there agree.
  const blockers = useMemo(() => blockedBy(nodes, assets ?? [], pins), [nodes, assets, pins])
  const rows = useMemo(() => buildRows(nodes.filter((n) => n.kind === kind), 0, blockers, pins), [nodes, kind, blockers, pins])
  const pipelines = useMemo(() => pipelineSummaries(rows, assets ?? [], kind), [rows, assets, kind])
  const groups = useGroups()
  const tree = useMemo(() => groupTree(groups.data ?? []), [groups.data])
  const leavesOf = useMemo(() => leavesByGroup(tree), [tree])
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes])
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

  // Each section's count, in the open pipeline: what its page lists.
  const counts = useMemo(() => {
    if (!assets) return {}
    const shown = assets.filter((a) => inPipeline(a.id, selected))
    const of = (k: NodeKind) => shown.filter((a) => a.kind === k).length
    return {
      assets: of('asset'),
      tasks: of('task'),
      sensors: of('sensor'),
      schedules: shown.filter((a) => a.freshness === 'schedule' && a.schedule !== undefined).length,
    }
  }, [assets, selected])

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
  const openGroup = (file: string, group: string) => {
    const next = new URLSearchParams({ pipeline: file, group })
    navigate({ pathname: '/assets', search: `?${next}` })
  }

  return (
    <SidebarView
      model={model}
      total={assets ? assets.filter((a) => a.kind === kind).length : null}
      counted={kind}
      pipelines={pipelines}
      groupsOf={(file) => fileGroups(file, tree, scope, leavesOf, byId)}
      selected={selected}
      filtering={FILTERABLE.includes(location.pathname)}
      scope={scope}
      onSelect={select}
      onOpenGroup={openGroup}
      dispatch={dispatch}
      search={<SidebarSearch onOpen={onSearch} />}
      nav={<SidebarNav counts={counts} search={selected ? `?pipeline=${encodeURIComponent(selected)}` : ''} extras={{ runs: <RunsInProgress /> }} />}
      footer={<SidebarFooter />}
    />
  )
}
