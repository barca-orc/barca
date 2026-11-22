import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'
import { RUN_DETAILS } from '@/lib/testdata/project'
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
