import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, it } from 'vitest'
import { CancelRun } from './RunActions'

function render(control: Parameters<typeof CancelRun>[0]['control']) {
  const client = new QueryClient()
  client.setQueryData(['health'], { read_only: false })
  return renderToStaticMarkup(<QueryClientProvider client={client}>
    <CancelRun id="run-1" queued={false} control={control} />
  </QueryClientProvider>)
}

describe('cancellation ownership', () => {
  it('offers cancellation for a server-owned live run', () => {
    expect(render({ cancellable: true, owner: 'this server', reason: null })).toContain('Cancel run')
  })

  it('shows where to stop a CLI-owned run instead of a cancel button', () => {
    const html = render({ cancellable: false, owner: 'process 42 on worker', reason: 'Stop it at process 42 on worker.' })
    expect(html).toContain('Stop it at process 42 on worker.')
    expect(html).not.toContain('<button')
  })
})
