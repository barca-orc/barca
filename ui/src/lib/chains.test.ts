import { describe, expect, it } from 'vitest'
import { byStep, chainSteps, resolveChain } from './chains'

const n = (id: string, inputs: string[] = []) => ({ id: `p.py:${id}`, inputs: inputs.map((i) => `p.py:${i}`) })
const nodes = [n('a'), n('b', ['a']), n('c', ['a']), n('x', ['c']), n('d', ['b', 'x']), n('e')]

describe('chains', () => {
  it('numbers steps and letters parallel ones', () => {
    const steps = chainSteps(['p.py:a', 'p.py:b', 'p.py:c', 'p.py:d'], nodes)
    expect(Object.fromEntries(steps)).toEqual({ 'p.py:a': '1', 'p.py:b': '2a', 'p.py:c': '2b', 'p.py:d': '3' })
    expect(byStep(steps)).toEqual(['p.py:a', 'p.py:b', 'p.py:c', 'p.py:d'])
  })
  it('orders through a non-member', () => {
    expect(Object.fromEntries(chainSteps(['p.py:c', 'p.py:d'], nodes))).toEqual({ 'p.py:c': '1', 'p.py:d': '2' })
  })
  it('resolves a range', () => {
    expect(resolveChain({ id: 'r', name: 'r', starts: ['c'], ends: ['d'] }, nodes)).toEqual(['p.py:c', 'p.py:x', 'p.py:d'])
    expect(resolveChain({ id: 'r', name: 'r', ends: ['b'] }, nodes)).toEqual(['p.py:a', 'p.py:b'])
  })
})

describe('unconnected lines', () => {
  it('numbers each line apart', () => {
    const steps = chainSteps(['p.py:a', 'p.py:b', 'p.py:e'], nodes)
    expect(Object.fromEntries(steps)).toEqual({ 'p.py:a': 'A1', 'p.py:b': 'A2', 'p.py:e': 'B1' })
    expect(byStep(steps)).toEqual(['p.py:a', 'p.py:b', 'p.py:e'])
  })
})
