import { describe, expect, it } from 'vitest'
import { fitTarget, fitsReadably, neighborInDirection, waitsOn, type ViewNode } from './graphView'

const INSET = { top: 32, right: 32, bottom: 64, left: 56 }
const node = (id: string, x: number, y: number, failed = false, inert = false): ViewNode =>
  ({ id, x, y, width: 208, height: 64, failed, inert })
/** A chain of `n` cards left to right, 304px apart (card plus rank gap). */
const chain = (n: number, failed: number[] = []) =>
  Array.from({ length: n }, (_, i) => node(`n${i}`, i * 304, 0, failed.includes(i)))

describe('fitTarget', () => {
  it('fits the whole graph when it is readable at 1:1', () => {
    const nodes = chain(3)
    expect(fitsReadably(nodes, 1192, 813, INSET)).toBe(true)
    expect(fitTarget(nodes, 1192, 813, INSET, null)).toEqual(nodes)
  })
  it('shows the selected node of a graph too wide for the screen', () => {
    const nodes = chain(7, [3, 6])
    expect(fitsReadably(nodes, 1192, 813, INSET)).toBe(false)
    expect(fitTarget(nodes, 1192, 813, INSET, 'n5').map((n) => n.id)).toEqual(['n5'])
  })
  it('shows the failed nodes together when they fit, else the first', () => {
    expect(fitTarget(chain(7, [3, 4]), 1192, 813, INSET, null).map((n) => n.id)).toEqual(['n3', 'n4'])
    expect(fitTarget(chain(7, [3, 6]), 1192, 813, INSET, null).map((n) => n.id)).toEqual(['n3'])
  })
  it('falls back to everything, and to everything before the screen is measured', () => {
    const nodes = chain(7)
    expect(fitTarget(nodes, 1192, 813, INSET, null)).toEqual(nodes)
    expect(fitTarget(chain(7, [6]), 0, 0, INSET, null)).toHaveLength(7)
  })
})

describe('neighborInDirection', () => {
  const nodes = [node('a', 0, 0), node('b', 304, -100, true), node('c', 304, 100), node('d', 608, 0), node('port', -300, 0, false, true)]
  it('moves to the nearest node ahead, preferring the one in line', () => {
    expect(neighborInDirection(nodes, 'a', 'ArrowRight')).toBe('b')
    expect(neighborInDirection(nodes, 'b', 'ArrowDown')).toBe('c')
    expect(neighborInDirection(nodes, 'c', 'ArrowRight')).toBe('d')
    expect(neighborInDirection(nodes, 'd', 'ArrowLeft')).toBe('b')
  })
  it('stays put at an edge of the graph and skips ports', () => {
    expect(neighborInDirection(nodes, 'd', 'ArrowRight')).toBeNull()
    expect(neighborInDirection(nodes, 'a', 'ArrowLeft')).toBeNull()
  })
  it('starts at the first failed node, else the upstream-most one', () => {
    expect(neighborInDirection(nodes, null, 'ArrowRight')).toBe('b')
    expect(neighborInDirection([node('a', 0, 0), node('b', 304, 0)], null, 'ArrowDown')).toBe('a')
  })
})

describe('waitsOn', () => {
  const name = (id: string) => id.split(':').pop()!
  it('names the failed input of a never-run consumer, as a hint beside its state', () => {
    const out = waitsOn([['p:publish', 'p:validate']], new Map([['p:publish', 'failed'], ['p:validate', 'waiting']]), name)
    expect(out.get('p:validate')).toEqual({ by: 'p:publish', hint: 'Blocked: waits on publish, whose last attempt failed.' })
  })
  it('follows a chain of never-run nodes to the failure behind them', () => {
    const out = waitsOn(
      [['a', 'b'], ['b', 'c'], ['x', 'c']],
      new Map([['a', 'failed'], ['b', 'waiting'], ['c', 'waiting'], ['x', 'other']]),
      name,
    )
    expect(out.get('c')?.hint).toBe('Blocked: waits on a, whose last attempt failed.')
    expect(out.size).toBe(2)
  })
  it('leaves nodes in other states, and unexplained never-run ones, alone', () => {
    const out = waitsOn(
      [['a', 'b'], ['a', 'c'], ['d', 'e']],
      new Map([['a', 'failed'], ['b', 'other'], ['c', 'other'], ['d', 'other'], ['e', 'waiting']]),
      name,
    )
    expect(out.size).toBe(0)
  })
})
