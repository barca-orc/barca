import { Moon, Sun } from 'lucide-react'
import { IconButton } from '@/components'
import { useTheme } from '@/context/theme'
import { Graph } from '@/kit/sections/Graph'
import { Pages } from '@/kit/sections/Pages'
import { Primitives } from '@/kit/sections/Primitives'
import { Runs } from '@/kit/sections/Runs'

const CONTENTS = [
  { id: 'kit-primitives', label: 'Primitives' },
  { id: 'kit-graph', label: 'Graph' },
  { id: 'kit-runs', label: 'Runs and panels' },
  { id: 'kit-pages', label: 'Pages' },
]

/**
 * Every component in each state it has, from the fixture tables in `kit/fixtures.ts`.
 * Development only (router.tsx): the place to look when changing one, and where a new one is
 * added with its states.
 */
export function KitPage() {
  const { theme, toggleTheme } = useTheme()
  return (
    <div className="barca-app"><div className="barca-main"><div className="barca-content">
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>Kit</h1>
            <span className="barca-count">components/ · development only</span>
          </div>
          <nav className="barca-kit-contents" aria-label="Kit contents">
            {CONTENTS.map(({ id, label }) => (
              <button key={id} type="button" onClick={() => document.getElementById(id)?.scrollIntoView({ block: 'start' })}>
                {label}
              </button>
            ))}
          </nav>
          <IconButton label={theme === 'dark' ? 'Light theme' : 'Dark theme'} size="sm" onClick={toggleTheme}>
            {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
          </IconButton>
        </div>
      </div>
      <div className="barca-view-body barca-kit">
        <div id="kit-primitives" />
        <Primitives />
        <Graph />
        <Runs />
        <Pages />
      </div>
    </div>
    </div></div></div>
  )
}
