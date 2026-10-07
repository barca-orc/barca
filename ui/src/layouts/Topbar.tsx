import { Fragment } from 'react'
import { Search, Sun, Moon, Bell, Settings, Activity, Slash, Play } from 'lucide-react'
import { Button, IconButton } from '@/components'
import { useTheme } from '@/context/theme'

import type { TopbarRun } from './shellContext'

interface TopbarProps {
  crumbs: string[]
  /** The current page's Run action; omitted when the page has nothing to run. */
  run?: TopbarRun | null
}

export function Topbar({ crumbs, run }: TopbarProps) {
  const { theme, toggleTheme } = useTheme()
  return (
    <header className="barca-topbar">
      <div className="barca-crumbs">
        {crumbs.map((c, i) => (
          <Fragment key={i}>
            {i > 0 && <Slash size={12} className="barca-crumb-sep" />}
            <span className={'barca-crumb' + (i === crumbs.length - 1 ? ' is-last' : '')}>
              {c}
            </span>
          </Fragment>
        ))}
      </div>

      <div className="barca-search">
        <Search size={13} />
        <span>Search assets, runs…</span>
        <kbd>⌘K</kbd>
      </div>

      <div className="barca-top-actions">
        <IconButton
          label={theme === 'light' ? 'Switch to dark' : 'Switch to light'}
          onClick={toggleTheme}
        >
          {theme === 'light' ? <Moon size={15} /> : <Sun size={15} />}
        </IconButton>
        <IconButton label="Activity">
          <Activity size={15} />
        </IconButton>
        <IconButton label="Notifications">
          <Bell size={15} />
        </IconButton>
        <IconButton label="Settings">
          <Settings size={15} />
        </IconButton>
        <div className="barca-divider-v" />
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
