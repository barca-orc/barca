import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { KEYS_OPEN_MAX, PartitionBar } from './NodeKeys'
import type { PartitionState } from '@/lib/types'

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
