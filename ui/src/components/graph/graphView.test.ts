import { describe, expect, it } from 'vitest'
import { blockedLabels, fitTarget, fitsReadably, neighborInDirection, type ViewNode } from './graphView'
import type { StatusKind } from '@/lib/types'

const INSET = { top: 32, right: 32, bottom: 64, left: 56 }
const node = (id: string, x: number, y: number, status: StatusKind = 'success', inert = false): ViewNode =>
  ({ id, x, y, width: 208, height: 64, status, inert })
/** A chain of `n` cards left to right, 304px apart (card plus rank gap). */
const chain = (n: number, failed: number[] = []) =>
  Array.from({ length: n }, (_, i) => node(`n${i}`, i * 304, 0, failed.includes(i) ? 'failed' : 'success'))

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
  const nodes = [node('a', 0, 0), node('b', 304, -100, 'failed'), node('c', 304, 100), node('d', 608, 0), node('port', -300, 0, 'skipped', true)]
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

describe('blockedLabels', () => {
  const name = (id: string) => id.split(':').pop()!
  it('names the failed input of a skipped consumer', () => {
    const out = blockedLabels([['p:publish', 'p:validate']], new Map([['p:publish', 'failed'], ['p:validate', 'skipped']]), name)
    expect(out.get('p:validate')).toEqual({ label: 'Blocked: publish failed', hint: 'Not run: its input publish failed.' })
  })
  it('follows a chain of skipped nodes to the failure behind them', () => {
    const out = blockedLabels(
      [['a', 'b'], ['b', 'c'], ['x', 'c']],
      new Map([['a', 'failed'], ['b', 'skipped'], ['c', 'skipped'], ['x', 'success']]),
      name,
    )
    expect(out.get('c')?.label).toBe('Blocked: a failed')
    expect(out.size).toBe(2)
  })
  it('leaves stale, healthy and unexplained skipped nodes alone', () => {
    const out = blockedLabels(
      [['a', 'b'], ['a', 'c'], ['d', 'e']],
      new Map([['a', 'failed'], ['b', 'warning'], ['c', 'success'], ['d', 'success'], ['e', 'skipped']]),
      name,
    )
    expect(out.size).toBe(0)
  })
})
