import { useState } from 'react'
import { Outlet } from 'react-router'
import { Sidebar } from './Sidebar'
import { Topbar } from './Topbar'
import type { AppShellContext, TopbarRun } from './shellContext'

export function AppShell() {
  const [topbarRun, setTopbarRun] = useState<TopbarRun | null>(null)

  return (
    <div className="barca-app">
      <Sidebar />
      <div className="barca-main">
        <Topbar run={topbarRun} />
        <div className="barca-content">
          <Outlet context={{ setTopbarRun } satisfies AppShellContext} />
        </div>
      </div>
    </div>
  )
}
