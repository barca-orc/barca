import { describe, expect, it } from 'vitest'
import { causeImpact, causes } from './causes'
import type { Failure } from './health'
import type { HealthTree } from './healthTree'

const tree = { nodes: new Map(), parent: new Map(), healthOf: new Map(), blockedBy: new Map([['p:b1', 'p:a'], ['p:b2', 'p:a'], ['p:b3', 'p:c']]) } as HealthTree
const failure = (id: string, headline: string, over: Partial<Failure> = {}): Failure => ({
  id, name: id, kind: 'asset', headline, error: headline, at: '2026-11-16 10:00:00', failingSince: '2026-11-16 09:00:00', failures: 3, impact: 0, missingKeys: null, ...over,
})

describe('causes', () => {
  it('groups failures by error, counts what each blocks, biggest first', () => {
    const list = causes({
      tree,
      failures: [
        failure('p:c', 'KeyError: 3'),
        failure('p:a', 'TypeError: x', { keys: { failed: 1205, total: 1205, errors: null, runId: null } }),
        failure('p:d', 'KeyError: 7'),
      ],
      suspect: new Map([['p:e', '3 of 10 keys empty']]),
      behind: new Map([['p:f', 'late by 12m']]),
    })
    expect(list.map((c) => c.kind)).toEqual(['failure', 'failure', 'suspect', 'behind'])
    const [first, second] = list
    expect(first).toMatchObject({ headline: 'TypeError: x', nodes: ['p:a'], failedKeys: 1205, blocked: 2 })
    expect(second).toMatchObject({ headline: 'KeyError: 3', nodes: ['p:c', 'p:d'], blocked: 1 })
    expect(causeImpact(first as Extract<typeof first, { kind: 'failure' }>)).toBe('1,205 keys failed · 2 blocked downstream')
    expect(causeImpact(second as Extract<typeof second, { kind: 'failure' }>)).toBe('2 nodes · 1 blocked downstream')
  })

  it('keeps to a scope', () => {
    const list = causes({ tree, failures: [failure('p:a', 'TypeError: x'), failure('p:c', 'KeyError: 3')], within: new Set(['p:c']) })
    expect(list).toHaveLength(1)
  })
})
