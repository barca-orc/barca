import { useState } from 'react'
import { Outlet, useLocation, useNavigate } from 'react-router'
import { Sidebar } from './Sidebar'
import { Topbar } from './Topbar'

export function AppShell() {
  const location = useLocation()
  const navigate = useNavigate()
  // Empty = "no explicit selection"; the sidebar defaults the highlight to the
  // first served pipeline.
  const [pipeline, setPipeline] = useState('')

  // Breadcrumbs from the path: "prod" root + the path segments.
  const segments = location.pathname.split('/').filter(Boolean)
  const crumbs = ['prod', ...(segments.length ? segments : ['graph'])]

  const onPipeline = (name: string) => {
    setPipeline(name)
    navigate('/graph')
  }

  return (
    <div className="barca-app">
      <Sidebar pipeline={pipeline} onPipeline={onPipeline} />
      <div className="barca-main">
        <Topbar crumbs={crumbs} onRun={() => navigate('/runs')} />
        <div className="barca-content">
          <Outlet />
        </div>
      </div>
    </div>
  )
}
