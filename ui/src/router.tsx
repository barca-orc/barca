import { createHashRouter, Navigate } from 'react-router'
import { AppShell } from '@/layouts/AppShell'
import { GraphPage } from '@/pages/GraphPage'
import { AssetsPage } from '@/pages/AssetsPage'
import { SchedulesPage } from '@/pages/SchedulesPage'
import { RunsPage, DocsPage } from '@/pages/placeholders'

// Hash routing: the page itself is always `<prefix>/ui/`, so relative asset URLs
// and the API base (see lib/apiBase.ts) work under any reverse-proxy prefix,
// and the server needs no deep-link fallback.
export const router = createHashRouter([
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <Navigate to="/assets" replace /> },
      { path: 'graph', element: <GraphPage /> },
      { path: 'runs', element: <RunsPage /> },
      { path: 'assets', element: <AssetsPage /> },
      { path: 'schedules', element: <SchedulesPage /> },
      { path: 'docs', element: <DocsPage /> },
    ],
  },
])
