import { createHashRouter, Navigate } from 'react-router'
import { AppShell } from '@/layouts/AppShell'
import { AssetsPage } from '@/pages/AssetsPage'
import { AssetsView, LegacyGraph } from '@/pages/AssetsView'
import { SchedulesPage } from '@/pages/SchedulesPage'
import { DocsPage } from '@/pages/placeholders'
import { RunsPage } from '@/pages/RunsPage'
import { KitPage } from '@/pages/KitPage'
import { TokensPage } from '@/pages/TokensPage'
import { AuthGate } from '@/context/AuthGate'

// Hash routing: the page itself is always `<prefix>/ui/`, so relative asset URLs
// and the API base (see lib/apiBase.ts) work under any reverse-proxy prefix,
// and the server needs no deep-link fallback.
export const router = createHashRouter([
  {
    path: '/',
    element: <AuthGate><AppShell /></AuthGate>,
    children: [
      { index: true, element: <Navigate to="/assets" replace /> },
      { path: 'graph', element: <LegacyGraph /> },
      { path: 'runs', element: <RunsPage /> },
      { path: 'assets', element: <AssetsView /> },
      { path: 'tasks', element: <AssetsPage kind="task" /> },
      { path: 'sensors', element: <AssetsPage kind="sensor" /> },
      { path: 'schedules', element: <SchedulesPage /> },
      { path: 'tokens', element: <TokensPage /> },
      { path: 'docs', element: <DocsPage /> },
    ],
  },
  // Every component in every state, from fixtures: development only, not in the built UI.
  // Outside the auth gate and the shell, so it renders with no server at all.
  ...(import.meta.env.DEV ? [{ path: '/kit', element: <KitPage /> }] : []),
])
