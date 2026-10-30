import { describe, it, expect } from 'vitest'
import { domainFile, domainMap } from './domainMap'
import type { ListNode } from './types'

const node = (id: string, inputs: string[] = []): ListNode => ({ id, kind: 'asset', freshness: 'always', env: [], file: 'p.py', line: 1, inputs })
const filler = (file: string, count: number) => Array.from({ length: count }, (_, i) => node(`${file}:n${i}`))

describe('domainMap', () => {
  it('draws one edge per pair of files, weighted by the inputs that cross it', () => {
    const map = domainMap([
      ...filler('a.py', 20),
      ...filler('b.py', 20),
      node('b.py:x', ['a.py:n0', 'a.py:n1', 'b.py:n0', 'gone.py:y']),
    ])!
    const b = map.entries.find((e) => e.id === 'domain:b.py')!
    expect(b.inputs).toEqual(['domain:a.py'])
    expect(b.inputWeights).toEqual({ 'domain:a.py': 2 })
    expect(b.summary).toBe('21 nodes · 1 in · 0 out')
    expect(map.leaves.get('domain:a.py')).toHaveLength(20)
    expect(b.loose).toBe(false)
  })

  it('is a sources-first grid when its edges are too many to draw', () => {
    const assets = Array.from({ length: 10 }, (_, f) =>
      Array.from({ length: 4 }, (_, n) => node(`f${f}.py:n${n}`, Array.from({ length: f }, (_, up) => `f${up}.py:n${n}`))),
    ).flat()
    const map = domainMap(assets)!
    expect(map.entries.every((e) => e.loose)).toBe(true)
    expect(map.entries[0]!.id).toBe('domain:f0.py')
    expect(map.entries.at(-1)!.id).toBe('domain:f9.py')
  })

  it('leaves a small project or a single file to its nodes', () => {
    expect(domainMap([...filler('a.py', 3), ...filler('b.py', 3)])).toBeNull()
    expect(domainMap(filler('a.py', 100))).toBeNull()
  })

  it('names the file behind a card', () => {
    expect(domainFile('domain:src/a.py')).toBe('src/a.py')
    expect(domainFile('group:a.py:g')).toBeNull()
  })
})
