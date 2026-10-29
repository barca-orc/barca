import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from 'react-router'
import { ThemeProvider } from '@/context/ThemeContext'
import { router } from '@/router'
import { AuthGate } from '@/context/AuthGate'
import { subscribeConnection } from '@/lib/credentials'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { refetchOnWindowFocus: false },
  },
})

// Clear project data before subscribers render a different connection.
const unsubscribe = subscribeConnection(() => queryClient.clear())
if (import.meta.hot) import.meta.hot.dispose(unsubscribe)

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <AuthGate><RouterProvider router={router} /></AuthGate>
      </ThemeProvider>
    </QueryClientProvider>
  )
}
