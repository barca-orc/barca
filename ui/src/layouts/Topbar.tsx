import { Fragment } from 'react'
import { Search, Sun, Moon, Bell, Settings, Activity, Slash, Play } from 'lucide-react'
import { Button, IconButton } from '@/components'
import { useTheme } from '@/context/theme'

interface TopbarProps {
  crumbs: string[]
  onRun?: () => void
}

export function Topbar({ crumbs, onRun }: TopbarProps) {
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
        <Button variant="signal" size="sm" iconLeft={<Play size={12} />} onClick={onRun}>
          Run
        </Button>
      </div>
    </header>
  )
}
