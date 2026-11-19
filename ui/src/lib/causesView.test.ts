import { describe, expect, it } from 'vitest'
import { ACTION_WORD, ROOT_KIND_WORD, actionWords, explained, heldCauses, heldGap, heldLine, signals, todos } from './causesView'
import { ROLLUP, entry, root } from './testdata/project'
import { causeCounts, failureRoot, heldBy, heldRoot } from './testdata/rollup'

// Roots spelled by hand, for the kinds and sizes the generated project has none of (a run
// queue, a sensor value, a Manual node, two failures with one error, hundreds of nodes): input
// to the joining and wording below, not a claim about what the server says.
const roots = [
  heldRoot({ kind: 'no_schedule', node: 'p.py:ledger', detail: 'no tick reaches ledger', nodes: 152, assets: 140, action: { kind: 'add_schedule', command: null, run_once: null, estimated_seconds: 420 } }),
  failureRoot('p.py:orders', 'row count fell 64%', 59, 52),
  failureRoot('p.py:refunds', 'row count fell 12%', 1),
  heldRoot({ kind: 'queue', node: 'p.py:pulse', detail: "pulse's ticks wait about 2m for a run slot, every 1m: max_runs is 1", nodes: 6, action: { kind: 'raise_max_runs', command: null, run_once: null, estimated_seconds: 18 } }),
  heldRoot({ kind: 'no_schedule', node: 'p.py:reviewed', detail: 'reviewed is Manual: no tick computes it', nodes: 1, action: { kind: 'run', command: 'barca get reviewed --refresh reviewed --server', run_once: null, estimated_seconds: null } }),
  heldRoot({ kind: 'sensor_value', node: 'p.py:inbox', detail: 'inbox observed a new value', nodes: 180, action: { kind: 'wait', command: null, run_once: null, estimated_seconds: 45 } }),
]
const scope = {
  not_fresh: 399,
  held_by: [
    heldBy('no_schedule:p.py:ledger', 152, 152, { assets: 140, causes: causeCounts({ no_refresh_path: 152 }), gap_seconds: 8_208_000, unbounded: 152 }),
    heldBy('failure:p.py:orders', 59, 211, { assets: 52, causes: causeCounts({ failing: 59 }), unbounded: 59 }),
    heldBy('failure:p.py:refunds', 1, 212, { causes: causeCounts({ failing: 1 }), unbounded: 1 }),
    heldBy('queue:p.py:pulse', 6, 218, { causes: causeCounts({ queued: 6 }), gap_seconds: 130 }),
    heldBy('no_schedule:p.py:reviewed', 1, 219, { causes: causeCounts({ no_refresh_path: 1 }), unbounded: 1 }),
    heldBy('sensor_value:p.py:inbox', 180, 399, { causes: causeCounts({ catching_up: 17, undecided: 163 }), gap_seconds: 7200, unbounded: 163 }),
  ],
}

describe('the to-do list', () => {
  it('lists the scope\'s roots in the order served, each with what it holds there, and names no root twice', () => {
    const list = todos(scope, roots)
    expect(list.map((t) => t.root.id)).toEqual(['no_schedule:p.py:ledger', 'failure:p.py:orders', 'queue:p.py:pulse', 'no_schedule:p.py:reviewed', 'sensor_value:p.py:inbox'])
    // Failures with one error are one entry, headed by the first served.
    expect(list[1]!.same.map((s) => s.root.id)).toEqual(['failure:p.py:refunds'])
    const listed = list.flatMap((t) => [t.root.id, ...t.same.map((s) => s.root.id)])
    expect(new Set(listed).size).toBe(scope.held_by.length)
    // What the entries hold is the scope's: every count is read, none added up.
    expect(list[0]!.held.nodes).toBe(152)
    expect(todos(undefined, roots)).toEqual([])
    // A root the response does not carry is not invented.
    expect(todos({ held_by: [heldBy('code:p.py:gone', 1, 1)] }, roots)).toEqual([])
  })

  it('says what a root holds in the scope, each count with its unit', () => {
    const [ledger, , , , inbox] = todos(scope, roots)
    expect(heldLine(ledger!)).toBe('holds 152 nodes (140 assets)')
    expect(heldCauses(ledger!)).toBe('152 no refresh path')
    expect(heldGap(ledger!)).toBe('95d of gap across nodes (node-time) · 152 with no estimate')
    expect(heldCauses(inbox!)).toBe('163 undecided · 17 catching up')
    expect(heldGap(inbox!)).toBe('2h of gap across nodes (node-time) · 163 with no estimate')
    // In a scope that holds only some of a root's nodes, both numbers are said.
    const part = todos({ held_by: [heldBy('sensor_value:p.py:inbox', 40, 40, { assets: 38 })] }, roots)[0]!
    expect(heldLine(part)).toBe('holds 40 nodes (38 assets) here, 180 in all')
    expect(heldGap(part)).toBe('')
  })

  it('says what clears a root in the server\'s words: the command, or what to do instead, and the compute', () => {
    const by = (id: string) => actionWords(roots.find((r) => r.id === id)!)
    expect(by('no_schedule:p.py:ledger')).toEqual({ word: 'add a schedule', command: null, hint: 'freshness=Schedule(...) on it or on a node above it', cost: 'about 7m of compute' })
    expect(by('failure:p.py:orders')).toEqual({ word: 'fix the failure', command: 'barca get orders --server', hint: null, cost: null })
    expect(by('queue:p.py:pulse')).toMatchObject({ word: 'raise max_runs', command: null, cost: 'about 18s of compute' })
    expect(by('queue:p.py:pulse').hint).toContain('--max-runs')
    expect(by('no_schedule:p.py:reviewed')).toMatchObject({ word: 'run', command: 'barca get reviewed --refresh reviewed --server', hint: null })
    expect(by('sensor_value:p.py:inbox')).toEqual({ word: 'wait', command: null, hint: null, cost: 'about 45s of compute' })
    // Every kind and action the server can send has a word.
    expect(Object.values(ROOT_KIND_WORD).every((word) => word.length > 0)).toBe(true)
    expect(Object.values(ACTION_WORD).every((word) => word.length > 0)).toBe(true)
  })

  it('says how much of the scope the head of the list explains, from the served running total', () => {
    expect(explained(scope)).toEqual({ roots: 3, nodes: 212, of: 399 })
    expect(explained(scope, 1)).toEqual({ roots: 1, nodes: 152, of: 399 })
    // Nothing to say when the head is the whole list.
    expect(explained({ not_fresh: 3, held_by: [heldBy('code:p.py:a', 3, 3)] })).toBeNull()
    expect(explained({ not_fresh: 0, held_by: [] })).toBeNull()
  })
})

describe('the to-do list of the generated project', () => {
  const list = todos(entry('root'), ROLLUP.roots)

  it('is the roots as served, in the order served: what needs someone and holds the most first', () => {
    expect(list.map((t) => t.root.id)).toEqual([
      'scheduler_off', 'failure:billing.py:invoices', 'failure:orders.py:sales_by_day', 'failure:billing.py:check__invoices',
      'no_schedule:orders.py:forecast', 'input:orders.py:orders_enriched',
    ])
    expect(list.map((t) => t.root.id)).toEqual(ROLLUP.roots.map((r) => r.id))
    // Three failures, three errors: none is folded under another.
    expect(list.every((t) => t.same.length === 0)).toBe(true)
    // A file's list is the roots its own entry names.
    expect(todos(entry('file:billing.py'), ROLLUP.roots).map((t) => t.root.id)).toEqual(['failure:billing.py:invoices', 'failure:billing.py:check__invoices'])
  })

  it('says what each root holds and what it costs in freshness, from the entry', () => {
    const by = (id: string) => list.find((t) => t.root.id === id)!
    const lines = (id: string) => [heldLine(by(id)), heldCauses(by(id)), heldGap(by(id))]
    expect(lines('failure:billing.py:invoices')).toEqual(['holds 3 nodes (3 assets)', '3 failing', '1d of gap across nodes (node-time) · 3 with no estimate'])
    // A failing task holds a node and no asset.
    expect(lines('failure:billing.py:check__invoices')).toEqual(['holds 1 node (0 assets)', '1 failing', '1d of gap across nodes (node-time) · 1 with no estimate'])
    expect(lines('scheduler_off')).toEqual(['holds 1 node (0 assets)', '1 no refresh path', '8h of gap across nodes (node-time) · 1 with no estimate'])
    // The queued run covers `orders_enriched`; `revenue_hourly` below it is promised no time.
    expect(lines('input:orders.py:orders_enriched')).toEqual(['holds 2 nodes (2 assets)', '2 queued', '10s of gap across nodes (node-time) · 1 with no estimate'])
  })

  it('says what clears each root in the server\'s words', () => {
    const by = (id: string) => actionWords(root(id))
    expect(by('failure:billing.py:invoices')).toEqual({ word: 'fix the failure', command: 'barca get invoices --server', hint: null, cost: 'about 8s of compute' })
    // A task is run, not got; nothing has timed it, so there is no cost.
    expect(by('failure:billing.py:check__invoices')).toEqual({ word: 'fix the failure', command: 'barca run check__invoices --server', hint: null, cost: null })
    expect(by('scheduler_off')).toEqual({ word: 'start a server', command: 'barca serve', hint: null, cost: 'about 3s of compute' })
    expect(by('no_schedule:orders.py:forecast')).toEqual({ word: 'add a schedule', command: null, hint: 'freshness=Schedule(...) on it or on a node above it', cost: null })
    expect(by('input:orders.py:orders_enriched')).toEqual({ word: 'wait', command: null, hint: null, cost: 'about 14s of compute' })
    expect(root('scheduler_off').detail).toBe('the running scheduler does not fire nightly_export: 1 schedule missed a slot')
    expect(root('failure:billing.py:check__invoices').error_group).toBe('AssertionError: # rows with a negative total')
  })

  it('says how much of the scope the head of the list explains', () => {
    expect(explained(entry('root'))).toEqual({ roots: 3, nodes: 6, of: 10 })
    expect(explained(entry('file:orders.py'))).toEqual({ roots: 3, nodes: 4, of: 5 })
    // Two roots hold all of billing.py: the head is the whole list.
    expect(explained(entry('file:billing.py'))).toBeNull()
  })
})

describe('data signals', () => {
  it('are listed apart from the roots: suspect data, then what is older than its input, kept to a scope', () => {
    const suspect = new Map([['p.py:a', 'empty output'], ['q.py:b', 'rows changed']])
    const behind = new Map([['p.py:c', 'older than its input a']])
    expect(signals({ suspect, behind }).map((s) => [s.node, s.text])).toEqual([
      ['p.py:a', 'suspect data: empty output'],
      ['q.py:b', 'suspect data: rows changed'],
      ['p.py:c', 'older than its input a'],
    ])
    expect(signals({ suspect, behind, within: new Set(['p.py:a']) })).toEqual([{ node: 'p.py:a', glyph: 'partial', text: 'suspect data: empty output' }])
    expect(signals({})).toEqual([])
  })
})
