import { useEffect, useState } from 'react'
import { NavLink, useSearchParams } from 'react-router'
import { Sun, Moon, Play, Search } from 'lucide-react'
import { Button, ConfirmDialog, ConnectionBadge, IconButton, RunsInProgress } from '@/components'
import { useTheme } from '@/context/theme'
import { useHealth } from '@/hooks/useHealth'
import { connection } from '@/lib/connection'

import { CommandPalette } from './CommandPalette'
import type { TopbarRun } from './shellContext'
import { NAV_ITEMS } from './nav'

interface TopbarProps {
  /** The current page's Run action; omitted when the page has nothing to run. */
  run?: TopbarRun | null
}

export function Topbar({ run }: TopbarProps) {
  const { theme, toggleTheme } = useTheme()
  const [confirming, setConfirming] = useState(false)
  const [palette, setPalette] = useState(false)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setPalette((open) => !open)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  const [params] = useSearchParams()
  const health = useHealth()
  const pipeline = params.get('pipeline')
  const keep = pipeline ? `?pipeline=${encodeURIComponent(pipeline)}` : ''
  const startRun = () => (run?.confirm ? setConfirming(true) : run?.onRun())
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
        <IconButton label="Search (⌘K)" onClick={() => setPalette(true)}>
          <Search size={15} />
        </IconButton>
        <ConnectionBadge connection={connection(health.data, health.isError)} />
        <RunsInProgress />
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
            onClick={startRun}
          >
            <span className="barca-top-run">{run.label}</span>
          </Button>
        )}
        {palette && <CommandPalette run={run} onRun={startRun} onClose={() => setPalette(false)} />}
        {run?.confirm && confirming && (
          <ConfirmDialog
            {...run.confirm}
            onCancel={() => setConfirming(false)}
            onConfirm={() => {
              setConfirming(false)
              run.onRun()
            }}
          />
        )}
      </div>
    </header>
  )
}
