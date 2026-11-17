import { NavLink, useLocation, useNavigate, useSearchParams } from 'react-router'
import { useRollup } from '@/hooks/useRollup'
import { useMemo, useRef, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from 'react'
import { BookOpen, ChevronDown, ChevronRight, KeyRound, Layers, LogOut, Moon, PanelLeft, Search, Sun } from 'lucide-react'
import { Button, ConnectionBadge, IconButton, RunsInProgress, StatusGlyph } from '@/components'
import { QueueStatus } from '@/components/QueueStatus'
import { useAssets } from '@/hooks/useAssets'
import { useGroupTree } from '@/hooks/useGroupTree'
import { useHealth } from '@/hooks/useHealth'
import { useProgram } from '@/hooks/useProgram'
import { useTheme } from '@/context/theme'
import { useTimeZone } from '@/context/timeZone'
import { connection } from '@/lib/connection'
import { fileShown, inPipeline, pipelineSummaries, type FileDisplay, type PipelineSummary } from '@/lib/pipeline'
import { ROOT, worstOf } from '@/lib/rollupView'
import { plural } from '@/lib/plural'
import { SIDEBAR_KEY, SIDEBAR_STEP, SIDEBAR_WIDTH, SIDEBAR_WIDTH_KEY, fileGroups, initialSidebar, listedPipelines, updateSidebar, type SidebarEffect, type SidebarGroup, type SidebarModel, type SidebarMsg } from '@/lib/sidebar'
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
  /** Every counted node of the project as one state: what the collapsed sidebar shows for the pipelines. */
  overall?: FileDisplay | null
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
 * Its right edge drags (or, focused, takes the arrow keys) to make it wider; a double click puts
 * it back. Collapsed, every section keeps its icon and its name in a tooltip, and the pipelines
 * are one line: all of them, with the project's one state.
 */
export function SidebarView({ model, total, counted = 'asset', pipelines, overall = null, groupsOf, selected, filtering = true, scope, onSelect, onOpenGroup, dispatch, search, nav, footer }: SidebarViewProps) {
  const word = KIND_WORD[counted]
  return (
    <aside
      className="barca-sidebar"
      aria-label="Sidebar"
      data-collapsed={model.collapsed || undefined}
      style={model.collapsed ? undefined : ({ '--sidebar-width': `${model.width}px` } as CSSProperties)}
    >
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
          <span className="barca-pipe-empty">{total === 0 ? `no ${word}s served` : 'no pipelines served'}</span>
        )}
        {/* Collapsed: one line for every pipeline, a glyph for the whole project, not a dot per file nobody can tell apart. */}
        {model.collapsed && pipelines.length > 0 && (
          <div className="barca-pipe-row">
            <button
              type="button"
              className="barca-pipe"
              title={`All pipelines · ${plural(total ?? 0, word)}${overall ? ` · ${overall.live ? overall.label : `worst: ${overall.label}`}` : ''}`}
              aria-label="All pipelines"
              onClick={() => onSelect(null)}
            >
              {overall ? <StatusGlyph glyph={overall.glyph} live={overall.live} size={12} /> : <Layers size={13} className="barca-pipe-icon" />}
            </button>
          </div>
        )}
        {!model.collapsed && pipelines.length > 1 && (
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
        {!model.collapsed && pipelines.map((p) => {
          const groups = groupsOf(p.file)
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
      {!model.collapsed && <SidebarEdge width={model.width} dispatch={dispatch} />}
    </aside>
  )
}

/**
 * The sidebar's right edge: drag it to resize, or focus it and use the arrow keys (Shift for a
 * bigger step, Home and End for the narrowest and the widest); a double click resets the width.
 */
function SidebarEdge({ width, dispatch }: { width: number; dispatch: Dispatch<SidebarMsg> }) {
  const drag = useRef<{ x: number; width: number } | null>(null)
  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { x: event.clientX, width }
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current) dispatch({ type: 'resized', width: drag.current.width + event.clientX - drag.current.x })
  }
  const onPointerUp = () => {
    if (!drag.current) return
    drag.current = null
    dispatch({ type: 'resize_ended' })
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? SIDEBAR_STEP * 4 : SIDEBAR_STEP
    const msg: SidebarMsg | null =
      event.key === 'ArrowLeft' ? { type: 'nudged', by: -step }
      : event.key === 'ArrowRight' ? { type: 'nudged', by: step }
      : event.key === 'Home' ? { type: 'width_set', width: SIDEBAR_WIDTH.min }
      : event.key === 'End' ? { type: 'width_set', width: SIDEBAR_WIDTH.max }
      : event.key === 'Enter' ? { type: 'width_reset' }
      : null
    if (!msg) return
    event.preventDefault()
    dispatch(msg)
  }
  return (
    <div
      className="barca-side-edge"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      aria-valuemin={SIDEBAR_WIDTH.min}
      aria-valuemax={SIDEBAR_WIDTH.max}
      aria-valuenow={width}
      title="Drag to resize; double-click to reset"
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onDoubleClick={() => dispatch({ type: 'width_reset' })}
      onKeyDown={onKeyDown}
    />
  )
}

/** A saved value, or null where this browser keeps none (private mode, storage blocked). */
function readSaved(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

/** Remember the sidebar's collapse and width in this browser; nothing lost but the memory when it cannot. */
function saveSidebar(effect: SidebarEffect) {
  try {
    if (effect.type === 'save_collapsed') localStorage.setItem(SIDEBAR_KEY, effect.collapsed ? '1' : '0')
    else localStorage.setItem(SIDEBAR_WIDTH_KEY, String(effect.width))
  } catch {
    /* Storage unavailable: the sidebar keeps its state until the page closes. */
  }
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
        <QueueStatus />
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
  const assetQuery = useAssets()
  const assets = assetQuery.data
  const location = useLocation()
  const kind = countedKind(location.pathname)
  // Every file's state and every group's dot is the whole project's rollup (`GET /state/rollup`):
  // a file reads its sums (`files`), the project the root entry. Nothing is summed here.
  const rollup = useRollup(null)
  const summaries = useMemo(() => pipelineSummaries(rollup.loaded ? rollup.tree.files : null, assets ?? [], kind), [rollup, assets, kind])
  // The project's one state, for the collapsed sidebar: none until the rollup has loaded.
  const overall = useMemo(() => {
    const root = rollup.loaded ? rollup.tree.entries.get(ROOT) : undefined
    return root && root.nodes > 0 ? fileShown({ running: root.running, worst: worstOf(root.health) }) : null
  }, [rollup])
  const tree = useGroupTree(null)
  const navigate = useNavigate()
  const [params] = useSearchParams()
  const [model, dispatch] = useProgram<SidebarModel, SidebarMsg, SidebarEffect>(
    () => initialSidebar({ collapsed: readSaved(SIDEBAR_KEY), width: readSaved(SIDEBAR_WIDTH_KEY) }),
    updateSidebar,
    saveSidebar,
  )
  // The selected pipeline lives in the URL (?pipeline=<file>) — the one source
  // the Assets table and the graph both read.
  const selected = params.get('pipeline')
  const pipelines = useMemo(() => listedPipelines(summaries, selected), [summaries, selected])
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
      overall={overall}
      groupsOf={(file) => fileGroups(file, tree, scope, rollup.tree.entries)}
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
