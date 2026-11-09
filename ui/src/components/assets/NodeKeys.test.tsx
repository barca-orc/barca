import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { NodeKeys, PartitionBar } from './NodeKeys'
import type { PartitionKeys, PartitionState } from '@/lib/types'

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
})

describe('NodeKeys', () => {
  const ID = 'planning.py:demand_forecast'
  const errors = (n: number): PartitionKeys => ({
    keys: [], total: 0, recorded: 3077, failed: 7,
    errors: Array.from({ length: n }, (_, i) => ({ error: `ValueError: problem ${i}`, keys: 5 - i })),
  })
  const render = (client: QueryClient, p = partitions({ cached: 3000, missing: 77, failed: 7 })) => renderToStaticMarkup(
    <QueryClientProvider client={client}><MemoryRouter>
      <NodeKeys id={ID} partitions={p} />
    </MemoryRouter></QueryClientProvider>,
  )

  it("names the failed keys' errors, each a link into the key explorer filtered to it", () => {
    const client = new QueryClient()
    client.setQueryData(['partition-keys', ID, { status: 'failed', limit: 1 }], errors(4))
    const html = render(client)
    expect(text(html)).toContain('5 ValueError: problem 0')
    expect(html).toContain('href="/keys?node=planning.py%3Ademand_forecast&amp;state=failed&amp;error=1"')
    expect(text(html)).toContain('and 1 more errors')
    expect(text(html)).toContain('Run computes only these 77 keys')
    expect(text(html)).toContain('See where they are')
  })

  it('offers the explorer for a node with nothing wrong', () => {
    const html = render(new QueryClient(), partitions({ cached: 3077, missing: 0, failed: 0 }))
    expect(text(html)).toContain('Explore all 3,077 keys')
    expect(text(html)).not.toContain('Run computes')
  })
})
