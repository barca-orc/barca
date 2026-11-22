import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'
import { NOW_MS, RUN_IDS, runDetail, scenario } from '@/lib/testdata/project'
import { RunOverviewView } from './RunOverview'

describe('live cached progress', () => {
  it('shows cached keys as completed steps while remaining work is running', () => {
    const cached = scenario('mostly cached in flight')
    const run = { ...runDetail(RUN_IDS.first), ...cached.run, explanation: cached.explanation }
    const html = renderToStaticMarkup(
      <MemoryRouter><RunOverviewView run={run} now={null} nowMs={NOW_MS} /></MemoryRouter>,
    )
    expect(html).toContain('99% done')
    expect(html).toContain('364 of 365 steps')
    expect(html).toContain('0 executed, 364 cached')
    expect(html).not.toContain('365 nodes')
    expect(cached.explanation.counts.planned).toBe('365 steps planned')
    expect(cached.line.steps).toBe('0 ok · 364 cached · 1 left')
  })
})
