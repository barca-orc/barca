import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BlastRadius } from './BlastRadius'
import { IncidentControls, PausedTag } from './IncidentControls'
import type { ListNode } from '@/lib/types'

const node = (id: string, inputs: string[] = []): ListNode => ({
  id, kind: 'asset', freshness: 'always', inputs, env: [], file: 'p.py', line: 1,
})
const graph = [node('p.py:raw'), node('p.py:clean', ['p.py:raw']), node('p.py:report', ['p.py:clean'])]

function render(ui: React.ReactNode, seed: (c: QueryClient) => void = () => {}) {
  const client = new QueryClient()
  seed(client)
  return renderToStaticMarkup(
    <QueryClientProvider client={client}><MemoryRouter>{ui}</MemoryRouter></QueryClientProvider>,
  ).replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ')
}

describe('BlastRadius', () => {
  it('lists the downstream nodes and the schedules that would compute them', () => {
    const shown = render(
      <BlastRadius id="p.py:raw" graph={graph} states={new Map()} paused={new Set(['p.py:report'])} onSelect={() => {}} />,
      (c) => c.setQueryData(['schedule'], [{ id: 'p.py:report', kind: 'asset', next_fire: null, paused: true }]),
    )
    expect(shown).toContain('Blast radius')
    expect(shown).toContain('2 nodes')
    expect(shown).toContain('clean')
    expect(shown).toContain('report')
    expect(shown).toContain('reads it')
    expect(shown).toContain('schedule paused')
  })
})

describe('IncidentControls', () => {
  it('offers to pause the asset and downstream, the file, the schedules and the queue', () => {
    const shown = render(<IncidentControls node={{ id: 'p.py:clean' }} graph={graph} readOnly={false} />, (c) => {
      c.setQueryData(['schedule'], [{ id: 'p.py:report', kind: 'asset', next_fire: 1, paused: false }])
      c.setQueryData(['pauses'], { total: 0, paused: [] })
    })
    expect(shown).toContain('Pause this asset and downstream')
    expect(shown).toContain('Pause pipeline (file)')
    expect(shown).toContain('Pause 1 related schedule')
    expect(shown).toContain('Pause the run queue')
  })

  it('offers to resume a paused asset', () => {
    const shown = render(<IncidentControls node={{ id: 'p.py:clean' }} graph={graph} readOnly={false} />, (c) => {
      c.setQueryData(['pauses'], { total: 1, paused: [{ id: 'p.py:clean', paused_at: 1, via: 'p.py:clean' }] })
    })
    expect(shown).toContain('Resume this asset')
    expect(shown).toContain('Resume it and downstream')
    expect(shown).not.toContain('Pause this asset and downstream')
  })

  it('labels a paused row', () => {
    expect(render(<PausedTag id="a" paused={new Set(['a'])} />)).toContain('paused')
    expect(render(<PausedTag id="b" paused={new Set(['a'])} />).trim()).toBe('')
  })
})
