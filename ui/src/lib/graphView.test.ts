import { describe, expect, it } from 'vitest'
import { fitTarget, fitViewport, fitsReadably, MIN_FIT_ZOOM, MIN_LABEL_PX, neighborInDirection, revealViewport, SMALLEST_LABEL_PX, type ViewNode } from './graphView'

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
  it('zooms out a little, never below 11px labels, to show a graph whole rather than cut it', () => {
    // 1,120px at 1:1 on a 1,032px canvas: it fits at 0.92; on 960px it would not.
    const nodes = chain(4)
    expect(fitsReadably(nodes, 1120, 720, INSET)).toBe(true)
    expect(fitTarget(nodes, 1120, 720, INSET, null)).toEqual(nodes)
    expect(fitsReadably(nodes, 1048, 720, INSET)).toBe(false)
  })
  it('shows the selected node of a graph too wide for the screen', () => {
    const nodes = chain(7, [3, 6])
    expect(fitsReadably(nodes, 1192, 813, INSET)).toBe(false)
    expect(fitTarget(nodes, 1192, 813, INSET, 'n5').map((n) => n.id)).toEqual(['n5'])
  })
  it('shows the failed nodes together when they fit, else the first', () => {
    expect(fitTarget(chain(7, [3, 4]), 1192, 813, INSET, null).map((n) => n.id)).toEqual(['n3', 'n4'])
    expect(fitTarget(chain(9, [2, 8]), 1192, 813, INSET, null).map((n) => n.id)).toEqual(['n2'])
  })
  it('then the nodes a run is working on, then the start of the graph; everything before the screen is measured', () => {
    const nodes = chain(7)
    expect(fitTarget(nodes, 1192, 813, INSET, null).map((n) => n.id)).toEqual(['n0'])
    const running = nodes.map((n) => (n.id === 'n5' ? { ...n, live: true } : n))
    expect(fitTarget(running, 1192, 813, INSET, null).map((n) => n.id)).toEqual(['n5'])
    expect(fitTarget(chain(7, [6]), 0, 0, INSET, null)).toHaveLength(7)
  })
})

describe('fitViewport', () => {
  it('never renders the smallest label below 11px, however big the graph', () => {
    expect(SMALLEST_LABEL_PX * MIN_FIT_ZOOM).toBeCloseTo(MIN_LABEL_PX)
    for (const n of [3, 7, 17, 60]) {
      for (const failed of [[], [n - 1]]) {
        const view = fitViewport(chain(n, failed), 1208, 800, INSET, null)!
        expect(view.zoom * SMALLEST_LABEL_PX).toBeGreaterThanOrEqual(MIN_LABEL_PX - 1e-9)
      }
    }
  })
  it('shows a graph that fits whole, at most 1:1, centred across', () => {
    const view = fitViewport(chain(3), 1192, 813, INSET, null)!
    expect(view.zoom).toBe(1)
    // 816px wide in 1,104px of room: 144px either side, after the left inset.
    expect(view.x).toBe(56 + 144)
  })
  it('centres a graph too big on its failure, and keeps the graph\'s edge at the inset', () => {
    const nodes = chain(17, [9])
    const view = fitViewport(nodes, 1208, 800, INSET, null)!
    expect(view.zoom).toBe(MIN_FIT_ZOOM)
    const centre = (9 * 304 + 104) * view.zoom + view.x
    expect(centre).toBeCloseTo(56 + (1208 - 56 - 32) / 2)
    // At the start of the graph, it is not centred past the left edge: the graph starts at the inset.
    const start = fitViewport(chain(17), 1208, 800, INSET, null)!
    expect(start.x).toBeCloseTo(56)
    expect(start.zoom).toBe(MIN_FIT_ZOOM)
  })
  it('shows the whole graph at any zoom when asked for the overview', () => {
    expect(fitViewport(chain(17), 1208, 800, INSET, null, true)!.zoom).toBeLessThan(0.3)
  })
  it('is nothing for an unmeasured screen or an empty graph', () => {
    expect(fitViewport(chain(3), 0, 0, INSET, null)).toBeNull()
    expect(fitViewport([], 800, 600, INSET, null)).toBeNull()
  })
})

describe('revealViewport', () => {
  const box = { x: 1000, y: 100, width: 208, height: 40 }
  const inset = { top: 24, right: 24, bottom: 64, left: 24 }
  it('pans just enough to bring a node a panel would cover into view, keeping the zoom', () => {
    // A 1,208px canvas narrowed to 808px by a 400px panel: the node at 1,000–1,208 is under it.
    const next = revealViewport({ x: 0, y: 0, zoom: 1 }, box, 808, 800, inset)!
    expect(next).toEqual({ x: 808 - 24 - 1208, y: 0, zoom: 1 })
  })
  it('leaves a node already in view where it is', () => {
    expect(revealViewport({ x: 0, y: 0, zoom: 1 }, box, 1400, 800, inset)).toBeNull()
  })
  it('brings in a node above or left of the view', () => {
    expect(revealViewport({ x: -1100, y: 0, zoom: 0.5 }, box, 800, 600, inset)).toEqual({ x: -1100 + (24 - (500 - 1100)), y: 0, zoom: 0.5 })
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
