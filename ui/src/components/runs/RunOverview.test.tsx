import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'
import { NOW_MS, RUN_IDS, runDetail, scenario, RUN_DETAILS } from '@/lib/testdata/project'
import { runHref } from '@/lib/running'
import { RunOverviewView } from './RunOverview'

const base = Object.values(RUN_DETAILS)[0]!

describe('retained run executions', () => {
  it('links each stopped execution from the original run with its number and stop reason', () => {
    const run = { ...base, stop_reason: 'server stopped before completion', previous_attempts: [
      { run_id: 'original-attempt-1', attempt: 1, stop_reason: 'server restart (requeued)' },
      { run_id: 'original-attempt-2', attempt: 2, stop_reason: 'server restart (requeued)' },
    ] }
    const html = renderToStaticMarkup(<MemoryRouter><RunOverviewView run={run} now={null} nowMs={0} /></MemoryRouter>)
    expect(html).toContain(`href="${runHref('original-attempt-1')}"`)
    expect(html).toContain(`href="${runHref('original-attempt-2')}"`)
    expect(html).toContain('Attempt 1: logs and steps')
    expect(html).toContain('Attempt 2: logs and steps')
    expect(html).toContain('Stopped: server restart (requeued)')
    expect(html).toContain('Current execution: attempt 3')
    expect(html).toContain('Attempt 3 stopped: server stopped before completion')
  })

  it('names the archived execution and links back to its accepted request', () => {
    const run = { ...base, parent_run_id: 'original', run_attempt: 1, stop_reason: 'server restart (requeued)' }
    const html = renderToStaticMarkup(<MemoryRouter><RunOverviewView run={run} now={null} nowMs={0} /></MemoryRouter>)
    expect(html).toContain('Attempt 1 stopped: server restart (requeued)')
    expect(html).toContain(`href="${runHref('original')}"`)
  })
})

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
