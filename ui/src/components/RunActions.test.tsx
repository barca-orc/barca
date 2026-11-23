import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, it } from 'vitest'
import { CancelRun, RunActions, RetryStep } from './RunActions'

function render(control: Parameters<typeof CancelRun>[0]['control'], readOnly: boolean | null = false) {
  const client = new QueryClient()
  if (readOnly !== null) client.setQueryData(['health'], { read_only: readOnly })
  return renderToStaticMarkup(<QueryClientProvider client={client}>
    <CancelRun id="run-1" queued={false} control={control} />
  </QueryClientProvider>)
}

describe('cancellation ownership', () => {
  it('keeps Cancel disabled with a reason while health is unknown', () => {
    const html = render({ cancellable: true, owner: 'this server', reason: null }, null)
    expect(html).toContain('Cancel run')
    expect(html).toContain('disabled')
    expect(html).toContain('Checking server write access')
  })

  it('hides write actions on a read-only server', () => {
    expect(render({ cancellable: true, owner: 'this server', reason: null }, true)).toBe('')
  })

  it('offers cancellation for a server-owned live run', () => {
    expect(render({ cancellable: true, owner: 'this server', reason: null })).toContain('Cancel run')
  })

  it('shows where to stop a CLI-owned run instead of a cancel button', () => {
    const html = render({ cancellable: false, owner: 'process 42 on worker', reason: 'Stop it at process 42 on worker.' })
    expect(html).toContain('Stop it at process 42 on worker.')
    expect(html).not.toContain('<button')
  })
})

it('explains unavailable replay for older history and disables both run and step actions', () => {
  const client = new QueryClient()
  client.setQueryData(['health'], { read_only: false })
  const run = { run_id: 'old', scope: null }
  const html = renderToStaticMarkup(<QueryClientProvider client={client}>
    <RunActions run={run} retryLabel="Re-run failed" onStarted={() => {}} />
    <RetryStep run={run} stepId="p.py:publish" onStarted={() => {}} />
  </QueryClientProvider>)
  expect(html).toContain('no recorded scope')
  expect(html.match(/disabled=""/g)).toHaveLength(3)
})
