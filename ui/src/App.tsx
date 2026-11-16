import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from 'react-router'
import { ThemeProvider } from '@/context/ThemeContext'
import { TimeZoneProvider } from '@/context/TimeZoneContext'
import { router } from '@/router'
import { subscribeConnection } from '@/lib/credentials'

const queryClient = new QueryClient({
  defaultOptions: {
    // A hidden tab polls nothing (every poll waits for the tab to be shown again); a shown one
    // polls at each query's own interval, fast only while a run is live (`lib/running.ts`).
    queries: { refetchOnWindowFocus: false, refetchIntervalInBackground: false },
  },
})

// Clear project data before subscribers render a different connection.
const unsubscribe = subscribeConnection(() => queryClient.clear())
if (import.meta.hot) import.meta.hot.dispose(unsubscribe)

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <TimeZoneProvider>
          <RouterProvider router={router} />
        </TimeZoneProvider>
      </ThemeProvider>
    </QueryClientProvider>
  )
}
