import { NavLink } from 'react-router'
import { Plus } from 'lucide-react'
import { IconButton, StatusDot } from '@/components'
import { NAV_ITEMS } from './nav'
import { useAssets } from '@/hooks/useAssets'
import { pipelinesFromAssets } from '@/lib/pipeline'
import markUrl from '@/assets/brand/mark.svg'

interface SidebarProps {
  pipeline: string
  onPipeline: (name: string) => void
}

export function Sidebar({ pipeline, onPipeline }: SidebarProps) {
  const { data: assets = [], isError } = useAssets()
  const pipelines = pipelinesFromAssets(assets)
  const connected = !isError && assets.length > 0
  // Default the highlight to the first served pipeline when none is selected.
  const activeName = pipeline || pipelines[0]?.name

  return (
    <aside className="barca-sidebar">
      <div className="barca-brand">
        <img src={markUrl} width="22" height="22" alt="" />
        <span className="barca-wordmark">barca</span>
        <span className="barca-env">prod</span>
      </div>

      <nav className="barca-navlist">
        {NAV_ITEMS.map((item) => {
          const Icon = item.icon
          return (
            <NavLink key={item.id} to={item.path} className="barca-nav">
              <Icon size={15} />
              <span>{item.label}</span>
            </NavLink>
          )
        })}
      </nav>

      <div className="barca-sect">
        <span>Pipelines</span>
        <IconButton label="New pipeline" size="sm">
          <Plus size={13} />
        </IconButton>
      </div>
      <div className="barca-pipes">
        {pipelines.length === 0 && (
          <span className="barca-pipe-empty">no pipelines served</span>
        )}
        {pipelines.map((p) => (
          <button
            key={p.file}
            className="barca-pipe"
            data-active={activeName === p.name ? '' : undefined}
            onClick={() => onPipeline(p.name)}
          >
            <StatusDot status={connected ? 'success' : 'queued'} size={6} />
            <span className="barca-pipe-name">{p.name}</span>
          </button>
        ))}
      </div>

      <div className="barca-side-foot">
        <div className="barca-user">
          <span className="barca-avatar">d</span>
          <div>
            <div className="barca-user-name">data-eng</div>
            <div className="barca-user-meta">workspace · acme</div>
          </div>
        </div>
      </div>
    </aside>
  )
}
