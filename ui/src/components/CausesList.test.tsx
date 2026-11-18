import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { CausesList, DataSignals } from './CausesList'
import { HealthMap } from './HealthMap'
import { signals, todos } from '@/lib/causesView'
import { indexRollup, ROOT } from '@/lib/rollupView'
import { causeCounts, failureRoot, heldBy, heldRoot, nodeEntry, rollup, scopeEntry } from '@/lib/testdata/rollup'

const text = (html: string) => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ')
const name = (id: string) => id.slice(id.lastIndexOf(':') + 1)
const noop = () => {}

// As `GET /state/rollup` answers: four roots, the first three holding 212 of 218 not-fresh nodes.
const roots = [
  heldRoot({ kind: 'no_schedule', node: 'p.py:ledger', detail: 'no tick reaches ledger', nodes: 152, assets: 140, action: { kind: 'add_schedule', command: null, run_once: null, estimated_seconds: 420 } }),
  failureRoot('p.py:orders', 'row count fell 64%', 59, 52),
  failureRoot('p.py:refunds', 'row count fell 12%', 1),
  heldRoot({ kind: 'queue', node: 'p.py:pulse', detail: "pulse's ticks wait about 2m for a run slot, every 1m: max_runs is 1", nodes: 6, action: { kind: 'raise_max_runs', command: null, run_once: null, estimated_seconds: null } }),
]
const scope = {
  not_fresh: 218,
  held_by: [
    heldBy('no_schedule:p.py:ledger', 152, 152, { assets: 140, causes: causeCounts({ no_refresh_path: 152 }), gap_seconds: 8_208_000, unbounded: 152 }),
    heldBy('failure:p.py:orders', 59, 211, { assets: 52, causes: causeCounts({ failing: 59 }), unbounded: 59 }),
    heldBy('failure:p.py:refunds', 1, 212, { causes: causeCounts({ failing: 1 }), unbounded: 1 }),
    heldBy('queue:p.py:pulse', 6, 218, { causes: causeCounts({ queued: 6 }), gap_seconds: 130 }),
  ],
}
const list = todos(scope, roots)
const render = (props: Partial<Parameters<typeof CausesList>[0]> = {}) =>
  renderToStaticMarkup(<CausesList todos={list} scope={scope} name={name} onOpen={noop} nowMs={Date.parse('2026-11-17T14:57:00Z')} {...props} />)

describe('CausesList', () => {
  it('lists each root as a to-do: what it is, why, what it holds, the gap, and what to do', () => {
    const shown = text(render())
    expect(shown).toContain('no schedule ledger no tick reaches ledger holds 152 nodes (140 assets)')
    expect(shown).toContain('152 no refresh path · 95d of gap across nodes (node-time) · 152 with no estimate')
    expect(shown).toContain('do add a schedule freshness=Schedule(...) on it or on a node above it about 7m of compute')
    // A failure: its error, the command to copy, and the failures that share the error under it.
    expect(shown).toContain('failure orders row count fell 64% holds 59 nodes (52 assets)')
    expect(shown).toContain('do fix the failure barca get orders --server')
    expect(shown).toContain('1 more failure with this error')
    expect(render()).toContain('aria-label="Copy command"')
    // In the order served, and no root twice.
    const html = render()
    expect([...html.matchAll(/data-root="([^"]+)"/g)].map((m) => m[1])).toEqual(['no_schedule:p.py:ledger', 'failure:p.py:orders', 'failure:p.py:refunds', 'queue:p.py:pulse'])
  })

  it('says how much the head of the list explains, and opens the rest in place', () => {
    expect(text(render())).toContain('The first 3 roots hold 212 of 218 nodes that are not fresh')
    const short = text(render({ limit: 1 }))
    expect(short).toContain('The first 1 roots hold 152 of 218')
    expect(short).toContain('2 more: show all 3')
    expect(short).not.toContain('run queue')
    // No link away to a failures tab: the list holds every root itself.
    expect(render({ limit: 1 })).not.toContain('Failures')
  })

  it('marks the picked root, and says a fresh scope is fresh', () => {
    expect(render({ selected: 'queue:p.py:pulse', onSelect: noop })).toMatch(/data-root="queue:p\.py:pulse"[^>]*data-selected="true"/)
    expect(text(renderToStaticMarkup(<CausesList todos={[]} scope={{ held_by: [], not_fresh: 0 }} name={name} onOpen={noop} nowMs={0} />))).toContain('Every node in this scope is fresh.')
  })
})

describe('DataSignals', () => {
  it('is a section of its own, and nothing when there is no signal', () => {
    const data = signals({ suspect: new Map([['p.py:a', 'empty output']]), behind: new Map([['p.py:c', 'older than its input a']]) })
    const shown = text(renderToStaticMarkup(<DataSignals signals={data} name={name} onOpen={noop} />))
    expect(shown).toContain('Data signals · 2')
    expect(shown).toContain('a suspect data: empty output')
    expect(renderToStaticMarkup(<DataSignals signals={[]} name={name} onOpen={noop} />)).toBe('')
  })
})

describe('HealthMap', () => {
  const tree = indexRollup(rollup([
    scopeEntry(ROOT, 'root', null, ['group:p.py:late', 'group:p.py:fine'], { behind: 14, undecided: 3, ok: 23 }, {
      causes: causeCounts({ overdue: 14, undecided: 3 }), not_fresh: 17, unbounded: 17, fresh_by: null,
    }),
    scopeEntry('group:p.py:late', 'group', ROOT, ['p.py:x'], { behind: 14, undecided: 3, ok: 3 }, { label: 'Late', causes: causeCounts({ overdue: 14, undecided: 3 }), not_fresh: 17, unbounded: 17 }),
    nodeEntry('p.py:x', 'group:p.py:late', 'behind', { cause: 'overdue' }),
    scopeEntry('group:p.py:fine', 'group', ROOT, [], { ok: 20 }, { label: 'Fine' }),
  ]))
  const map = (focus = ROOT) => renderToStaticMarkup(<HealthMap tree={tree} focus={focus} onFocus={noop} onOpen={noop} />)

  it('says the scope in the terms of the gap at the centre, in nodes, with one word for grey', () => {
    const shown = text(map())
    expect(shown).toContain('14 overdue')
    expect(shown).toContain('3 undecided')
    expect(shown).toContain('40 nodes, 17 not fresh')
    expect(shown).toContain('17 with no estimate')
    for (const old of ['not known yet', 'waiting', 'needs attention']) expect(shown).not.toContain(old)
    // The key names every colour by its cause, and fresh.
    for (const word of ['failing', 'overdue', 'no refresh path', 'queued', 'undecided', 'catching up', 'fresh']) expect(shown).toContain(word)
  })

  it('draws a group as what is inside it, and a fresh group as fresh', () => {
    const html = map()
    expect(html).toMatch(/aria-label="Late: 14 overdue · 3 undecided, 20 nodes"[^>]*>(<path[^>]*data-part="overdue"[^>]*><\/path>)(<path[^>]*data-part="undecided"[^>]*><\/path>)(<path[^>]*data-part="fresh"[^>]*><\/path>)/)
    expect(html).toContain('aria-label="Fine: every node is fresh, 20 nodes"')
    expect(text(map('group:p.py:fine'))).toContain('20 nodes, all fresh')
  })
})
