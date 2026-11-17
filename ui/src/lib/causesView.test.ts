import { describe, expect, it } from 'vitest'
import { ACTION_WORD, ROOT_KIND_WORD, causeImpact, causes, heldImpact } from './causesView'
import { failureRoot, heldRoot } from './testdata/rollup'
import type { Failure } from './health'

const failure = (id: string, headline: string, held: number, over: Partial<Failure> = {}): Failure => ({
  id, name: id, kind: 'asset', headline, error: headline, group: headline.replace(/\d+/g, '#'), at: null, failingSince: null, failures: 1, held, heldAssets: held, missingKeys: null, ...over,
})

describe('causes', () => {
  // As served: the failure holding the most nodes first.
  const list = [
    failure('p.py:orders', 'row count fell 64%', 59, { heldAssets: 52, failingSince: '2026-11-16 09:00:00', keys: { failed: 1205, total: 3077, errors: null, runId: null } }),
    failure('p.py:audit', "KeyError: 'id'", 3),
    failure('p.py:refunds', 'row count fell 12%', 1),
  ]

  it('lists one entry per error group in served order, with its first failure\'s own numbers', () => {
    const out = causes({ failures: list })
    expect(out).toEqual([
      { kind: 'failure', headline: 'row count fell 64%', node: 'p.py:orders', others: 1, failedKeys: 1205, held: 59, heldAssets: 52, failingSince: '2026-11-16 09:00:00' },
      { kind: 'failure', headline: "KeyError: 'id'", node: 'p.py:audit', others: 0, failedKeys: 0, held: 3, heldAssets: 3, failingSince: null },
    ])
    // Every count names its unit, and nothing is added up across the group.
    expect(causeImpact(out[0] as never)).toBe('1,205 keys failed · holds 59 nodes (52 assets)')
    expect(causeImpact(out[1] as never)).toBe('holds 3 nodes (3 assets)')
    expect(causeImpact({ ...(out[1] as object), held: 1 } as never)).toBe('')
  })

  it('lists the roots that are not failures as served, in the server\'s words', () => {
    const roots = [
      failureRoot('p.py:orders', 'row count fell 64%', 59),
      heldRoot({ kind: 'no_schedule', node: 'p.py:ledger', detail: 'no tick reaches ledger', nodes: 152, assets: 140, action: { kind: 'add_schedule', command: null, estimated_seconds: 420 } }),
      heldRoot({ kind: 'queue', node: 'p.py:pulse', detail: "pulse's ticks wait about 2m for a run slot, every 1m: max_runs is 1", nodes: 6, action: { kind: 'raise_max_runs', command: null, estimated_seconds: 18 } }),
      heldRoot({ kind: 'no_schedule', node: 'p.py:reviewed', detail: 'reviewed is Manual: no tick computes it', nodes: 1, action: { kind: 'run', command: 'barca get reviewed --refresh reviewed --server', estimated_seconds: null } }),
    ]
    // A failure root is listed by `failures`, with its error; the rest follow in the order served.
    const out = causes({ failures: [list[0]!], roots })
    expect(out.map((c) => c.kind)).toEqual(['failure', 'held', 'held', 'held'])
    expect(out[1]).toEqual({ kind: 'held', node: 'p.py:ledger', what: 'no schedule', detail: 'no tick reaches ledger', held: 152, heldAssets: 140, action: 'add a schedule', command: null })
    expect(heldImpact(out[1] as never)).toBe('holds 152 nodes (140 assets)')
    expect(out[2]).toMatchObject({ what: 'run queue', action: 'raise max_runs' })
    expect(out[3]).toMatchObject({ what: 'no schedule', action: 'run', command: 'barca get reviewed --refresh reviewed --server' })
    // Every kind and action the server can send has a word.
    expect(Object.values(ROOT_KIND_WORD).every((word) => word.length > 0)).toBe(true)
    expect(Object.values(ACTION_WORD).every((word) => word.length > 0)).toBe(true)
  })

  it('adds suspect data, then what is behind, and keeps signals to a scope', () => {
    const suspect = new Map([['p.py:a', 'empty output'], ['q.py:b', 'rows changed']])
    const behind = new Map([['p.py:c', 'older than its input a']])
    expect(causes({ failures: [], suspect, behind }).map((c) => c.kind)).toEqual(['suspect', 'suspect', 'behind'])
    expect(causes({ failures: [], suspect, behind, within: new Set(['p.py:a']) })).toEqual([{ kind: 'suspect', node: 'p.py:a', detail: 'empty output' }])
  })
})
