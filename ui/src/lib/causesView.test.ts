import { describe, expect, it } from 'vitest'
import { causeImpact, causes } from './causesView'
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

  it('adds suspect data, then what is behind, and keeps signals to a scope', () => {
    const suspect = new Map([['p.py:a', 'empty output'], ['q.py:b', 'rows changed']])
    const behind = new Map([['p.py:c', 'older than its input a']])
    expect(causes({ failures: [], suspect, behind }).map((c) => c.kind)).toEqual(['suspect', 'suspect', 'behind'])
    expect(causes({ failures: [], suspect, behind, within: new Set(['p.py:a']) })).toEqual([{ kind: 'suspect', node: 'p.py:a', detail: 'empty output' }])
  })
})
