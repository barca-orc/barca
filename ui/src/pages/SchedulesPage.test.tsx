import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { QueryClient, QueryObserver, environmentManager } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { assetsQuery } from '@/hooks/useAssets'
import { RECOVER_MS } from '@/hooks/polling'
import { remote } from '@/lib/remote'
import type { Health, ListNode } from '@/lib/types'
import { SchedulesView, type SchedulesViewProps } from './SchedulesView'

const nightly: ListNode = {
  id: 'planning.py:publish_plan', kind: 'task', freshness: 'schedule', schedule: '0 6 * * *',
  env: [], inputs: [], file: 'planning.py', line: 40,
}

const text = (html: string) => html.replace(/<[^>]+>/g, '').replaceAll('&#x27;', "'")
const noop = () => {}
const page = (assets: SchedulesViewProps['assets'], offline = false) =>
  text(renderToStaticMarkup(
    <SchedulesView
      title="Schedules" pipeline={null} assets={assets} states={[]} dataUpdatedAt={0} jobs={[]}
      health={{ version: '0.39.0', read_only: false, scheduler: true } as Health}
      offline={offline} toggle={{ busyId: null, error: null }} onOpen={noop} onToggle={noop} onRetry={noop}
    />,
  ))

/** The server's answer is in, not the mock placeholder (which counts as success). */
const loaded = (observer: { getCurrentResult: () => { isSuccess: boolean; isPlaceholderData: boolean } }) => {
  const r = observer.getCurrentResult()
  return r.isSuccess && !r.isPlaceholderData
}

const ok = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } })

describe('SchedulesPage after a server restart', () => {
  const fetchMock = vi.fn<typeof fetch>()
  let client: QueryClient
  beforeEach(() => {
    vi.useFakeTimers()
    // Timers (refetch intervals) are off on a server; this test is the browser.
    environmentManager.setIsServer(() => false)
    vi.stubGlobal('fetch', fetchMock)
    client = new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } })
  })
  afterEach(() => {
    client.clear()
    environmentManager.setIsServer(() => typeof window === 'undefined')
    vi.unstubAllGlobals()
    vi.useRealTimers()
    fetchMock.mockReset()
  })

  it('shows the failure with a Retry, then the schedules once the server answers again', async () => {
    // The server is down for the first fetch and back for the next: the sequence of a restart.
    fetchMock.mockRejectedValueOnce(new TypeError('Failed to fetch')).mockImplementation(() => Promise.resolve(ok([nightly])))
    const observer = new QueryObserver(client, assetsQuery())
    const unsubscribe = observer.subscribe(noop)

    await vi.waitFor(() => expect(observer.getCurrentResult().isError).toBe(true))
    const failed = page(remote(observer.getCurrentResult()))
    expect(failed).toContain("Can't load schedules: Failed to fetch")
    expect(failed).toContain('Retry')
    // Nothing is counted next to "can't load": a count there would read as current.
    expect(failed).not.toContain('scheduled')
    expect(failed).not.toContain('scheduler running')

    // No reload, no click: the failed query asks again on its own.
    await vi.advanceTimersByTimeAsync(RECOVER_MS)
    await vi.waitFor(() => expect(loaded(observer)).toBe(true))
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(fetchMock.mock.calls.every(([path]) => String(path).endsWith('/assets'))).toBe(true)
    const recovered = page(remote(observer.getCurrentResult()))
    expect(recovered).not.toContain("Can't load schedules")
    expect(recovered).toContain('publish_plan')
    expect(recovered).toContain('daily at 06:00')
    expect(recovered).toContain('1 scheduled')
    expect(recovered).toContain('scheduler running')
    unsubscribe()
  })

  it('keeps the loaded schedules when a later poll fails, and says they may be stale', async () => {
    fetchMock.mockResolvedValueOnce(ok([nightly])).mockRejectedValue(new TypeError('Failed to fetch'))
    const observer = new QueryObserver(client, { ...assetsQuery(), staleTime: 0 })
    const unsubscribe = observer.subscribe(noop)
    await vi.waitFor(() => expect(loaded(observer)).toBe(true))
    await observer.refetch()
    const result = observer.getCurrentResult()
    expect(result.isError).toBe(true)
    expect(result.data).toHaveLength(1)

    const shown = page(remote(result), true)
    expect(shown).toContain('publish_plan')
    expect(shown).toContain('Showing the last loaded schedules; they may be out of date: Failed to fetch')
    expect(shown).not.toContain('1 scheduled')
    expect(shown).not.toContain('scheduler running')
    unsubscribe()
  })
})
