import { useState } from 'react'
import { Outlet, useLocation } from 'react-router'
import { Sidebar } from './Sidebar'
import { Topbar } from './Topbar'
import { LoadErrors } from './LoadErrors'
import type { AppShellContext, TopbarRun } from './shellContext'

export function AppShell() {
  const location = useLocation()
  const [topbarRun, setTopbarRun] = useState<TopbarRun | null>(null)
  // Breadcrumbs from the path: "prod" root + the path segments.
  const segments = location.pathname.split('/').filter(Boolean)
  const crumbs = ['prod', ...(segments.length ? segments : ['graph'])]

  return (
    <div className="barca-app">
      <Sidebar />
      <div className="barca-main">
        <Topbar crumbs={crumbs} run={topbarRun} />
        <LoadErrors />
        <div className="barca-content">
          <Outlet context={{ setTopbarRun } satisfies AppShellContext} />
        </div>
      </div>
    </div>
  )
}
