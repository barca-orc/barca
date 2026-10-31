import { afterEach, describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { KEYS_OPEN_MAX, NodeKeys, PartitionBar } from './NodeKeys'
import { keyParam } from '@/lib/nodeDetail'
import type { PartitionKeyRow, PartitionKeys, PartitionState } from '@/lib/types'

const text = (html: string) => html.replace(/<[^>]+>/g, '')

const partitions = (over: Partial<PartitionState>): PartitionState => ({
  total: 3077, cached: 3070, missing: 7, missing_keys: [], failed: 7, ...over,
})

describe('PartitionBar', () => {
  it('names the failed keys in the failure colour', () => {
    const html = renderToStaticMarkup(<PartitionBar partitions={partitions({})} />)
    expect(text(html)).toContain('3,070 of 3,077 cached · 7 failed')
    expect(html).toMatch(/<span class="barca-cell-failed">7 failed<\/span>/)
  })

  it('draws 7 failed keys of 3,077 wide enough to see', () => {
    const html = renderToStaticMarkup(<PartitionBar partitions={partitions({})} />)
    expect(html).toMatch(/class="is-failed" style="width:2%"/)
    expect(html).not.toContain('is-stale')
    expect(html).toContain('aria-label="3070 cached, 7 failed, 0 stale, of 3077 keys"')
  })

  it('counts stale keys as the rest', () => {
    const html = renderToStaticMarkup(<PartitionBar partitions={partitions({ cached: 3000, missing: 77, failed: 7 })} />)
    expect(text(html)).toContain('3,000 of 3,077 cached · 7 failed · 70 stale')
    expect(html).toContain('class="is-stale"')
  })

  it('the key list starts open only for a handful of failures', () => {
    expect(KEYS_OPEN_MAX).toBe(10)
  })
})

describe('keyParam', () => {
  it('reads the key a palette result opens the panel with', () => {
    expect(keyParam('#/assets?node=planning.py%3Ademand_forecast&key=ra-00001')).toBe('ra-00001')
    expect(keyParam('#/assets?node=planning.py%3Ademand_forecast')).toBe('')
    expect(keyParam('#/assets')).toBe('')
    expect(keyParam('')).toBe('')
  })
})

describe('NodeKeys with a pre-filled key', () => {
  const ID = 'planning.py:demand_forecast'
  const row = (key: string, status: string): PartitionKeyRow => ({
    key, status, created_at: '2026-10-31 15:23:56', elapsed_seconds: 0.001, error: null, run_id: null,
    artifact_path: status === 'success' ? `.barca/artifacts/demand_forecast_${key}/abc.json` : null, size_bytes: 294,
  })
  const page = (keys: PartitionKeyRow[], total = keys.length): PartitionKeys => ({ keys, total, recorded: 3077, failed: 7, errors: [] })
  const hadWindow = 'window' in globalThis
  afterEach(() => { if (!hadWindow) delete (globalThis as { window?: unknown }).window })

  it('searches the key from the URL and, when no failed key matches, widens to every key', () => {
    // The panel opened from a palette result for a healthy key: "failed only" is on (7 failed).
    ;(globalThis as { window?: unknown }).window = { location: { hash: '#/assets?node=planning.py%3Ademand_forecast&key=ra-00001' } }
    const client = new QueryClient()
    // What the server answered: nothing failed matches, one key does.
    client.setQueryData(['partition-keys', ID, { status: 'failed', q: 'ra-00001', limit: 100 }], page([]))
    client.setQueryData(['partition-keys', ID, { q: 'ra-00001', limit: 100 }], page([row('account=ra-00001', 'success')]))
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <NodeKeys id={ID} partitions={partitions({})} nowMs={Date.parse('2026-10-31T15:30:00Z')} />
      </QueryClientProvider>,
    )
    expect(html).toContain('value="ra-00001"')
    expect(text(html)).toContain('No failed key matches; showing every key.')
    expect(text(html)).toContain('account=ra-00001')
    expect(text(html)).toContain('.barca/artifacts/demand_forecast_account=ra-00001/abc.json')
    expect(html).toContain('aria-pressed="false"')
  })

  it('keeps "failed only" when a failed key matches', () => {
    ;(globalThis as { window?: unknown }).window = { location: { hash: '#/assets?node=x&key=ra-02541' } }
    const client = new QueryClient()
    client.setQueryData(['partition-keys', ID, { status: 'failed', q: 'ra-02541', limit: 100 }], page([row('account=ra-02541', 'failed')]))
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <NodeKeys id={ID} partitions={partitions({})} nowMs={Date.parse('2026-10-31T15:30:00Z')} />
      </QueryClientProvider>,
    )
    expect(text(html)).not.toContain('showing every key')
    expect(text(html)).toContain('account=ra-02541')
    expect(html).toContain('aria-pressed="true"')
  })
})
