import { Outlet, useLocation, useNavigate } from 'react-router'
import { Sidebar } from './Sidebar'
import { Topbar } from './Topbar'

export function AppShell() {
  const location = useLocation()
  const navigate = useNavigate()
  // Breadcrumbs from the path: "prod" root + the path segments.
  const segments = location.pathname.split('/').filter(Boolean)
  const crumbs = ['prod', ...(segments.length ? segments : ['graph'])]

  return (
    <div className="barca-app">
      <Sidebar />
      <div className="barca-main">
        <Topbar crumbs={crumbs} onRun={() => navigate('/runs')} />
        <div className="barca-content">
          <Outlet />
        </div>
      </div>
    </div>
  )
}
