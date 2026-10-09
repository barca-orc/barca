import { NavLink, useLocation, useSearchParams } from 'react-router'
import { Sun, Moon, Play, List, GitBranch } from 'lucide-react'
import { Button, IconButton } from '@/components'
import { useTheme } from '@/context/theme'

import type { TopbarRun } from './shellContext'
import { NAV_ITEMS } from './nav'

interface TopbarProps {
  /** The current page's Run action; omitted when the page has nothing to run. */
  run?: TopbarRun | null
}

export function Topbar({ run }: TopbarProps) {
  const { theme, toggleTheme } = useTheme()
  const location = useLocation()
  const [params, setParams] = useSearchParams()
  const pipeline = params.get('pipeline')
  const keep = pipeline ? `?pipeline=${encodeURIComponent(pipeline)}` : ''
  const isAssets = location.pathname === '/assets'
  const graph = params.get('view') === 'graph'
  const setView = (view: 'list' | 'graph') => {
    const next = new URLSearchParams(params)
    const selected = next.get(graph ? 'focus' : 'node')
    next.delete('focus')
    next.delete('node')
    if (selected) next.set(view === 'graph' ? 'focus' : 'node', selected)
    if (view === 'graph') next.set('view', 'graph')
    else next.delete('view')
    setParams(next, { replace: true })
  }
  return (
    <header className="barca-topbar">
      <nav className="barca-primary-nav" aria-label="Main navigation">
        {NAV_ITEMS.map(item => (
          <NavLink key={item.id} to={{ pathname: item.path, search: keep }} className="barca-primary-link">
            {item.label}
          </NavLink>
        ))}
      </nav>

      <div className="barca-top-actions">
        {isAssets && (
          <div className="barca-view-toggle" role="group" aria-label="Asset view">
            <button type="button" aria-pressed={!graph} onClick={() => setView('list')}><List size={14} />List</button>
            <button type="button" aria-pressed={graph} onClick={() => setView('graph')}><GitBranch size={14} />Graph</button>
          </div>
        )}
        <IconButton
          label={theme === 'light' ? 'Switch to dark' : 'Switch to light'}
          onClick={toggleTheme}
        >
          {theme === 'light' ? <Moon size={15} /> : <Sun size={15} />}
        </IconButton>
        {run && (
          <Button
            variant="signal"
            size="sm"
            iconLeft={<Play size={12} />}
            loading={run.loading}
            disabled={run.disabled}
            title={run.title}
            onClick={run.onRun}
          >
            Run
          </Button>
        )}
      </div>
    </header>
  )
}
