import { describe, it, expect } from 'vitest'
import { lineage } from './lineage'

// a → b → c → d → e, and x → c, b → y
const graph = [
  { id: 'a', inputs: [] }, { id: 'b', inputs: ['a'] }, { id: 'x', inputs: [] },
  { id: 'c', inputs: ['b', 'x'] }, { id: 'd', inputs: ['c'] }, { id: 'e', inputs: ['d'] },
  { id: 'y', inputs: ['b'] },
]

describe('lineage', () => {
  it('keeps the node and its ancestors and descendants within depth, not siblings', () => {
    expect(lineage('c', graph, 1).map((n) => n.id)).toEqual(['b', 'x', 'c', 'd'])
    expect(lineage('c', graph, 2).map((n) => n.id)).toEqual(['a', 'b', 'x', 'c', 'd', 'e'])
  })

  it('is empty for an unknown node', () => {
    expect(lineage('nope', graph)).toEqual([])
  })
})
