import { describe, expect, it } from 'vitest'
import { ROOT, arcPath, freshWithin, healthSummary, heldIds, indexRollup, leafIds, nearestInTree, noEstimate, ringRadii, rootsWithin, sunburst, treePath, worstOf } from './rollupView'
import { causeCounts, counts, failureRoot, heldBy, nodeEntry, rollup, scopeEntry } from './testdata/rollup'

// a.py: raw (failing, 100 of 120 keys failed) → clean, report (blocked by it)
// b.py: src (ok) → mart (never run, no refresh path); c.py: x (behind, catching up), waits (undecided)
const byFile = indexRollup(rollup([
  scopeEntry(ROOT, 'root', null, ['file:a.py', 'file:c.py', 'file:b.py'], { failing: 1, blocked: 2, behind: 1, undecided: 1, never_run: 1, ok: 1 }, {
    causes: causeCounts({ failing: 3, no_refresh_path: 1, undecided: 1, catching_up: 1 }), not_fresh: 6, unbounded: 5, fresh_by: '2026-11-17T15:01:10Z',
    held_by: [heldBy('failure:a.py:raw', 3, 3), heldBy('no_schedule:b.py:mart', 1, 4), heldBy('undecided:c.py:waits', 1, 5), heldBy('code:c.py:x', 1, 6)],
  }),
  scopeEntry('file:a.py', 'file', ROOT, ['a.py:raw', 'a.py:clean', 'a.py:report'], { failing: 1, blocked: 2 }, { causes: causeCounts({ failing: 3 }), not_fresh: 3, held_by: [heldBy('failure:a.py:raw', 3, 3)] }),
  nodeEntry('a.py:raw', 'file:a.py', 'failing', { cause: 'failing', keys: { failed: 100, behind: 0, ok: 20 } }),
  nodeEntry('a.py:clean', 'file:a.py', 'blocked', { cause: 'failing' }),
  nodeEntry('a.py:report', 'file:a.py', 'blocked', { cause: 'failing' }),
  scopeEntry('file:c.py', 'file', ROOT, ['c.py:x', 'c.py:waits'], { behind: 1, undecided: 1 }, { causes: causeCounts({ undecided: 1, catching_up: 1 }), not_fresh: 2, held_by: [heldBy('undecided:c.py:waits', 1, 1), heldBy('code:c.py:x', 1, 2)] }),
  nodeEntry('c.py:x', 'file:c.py', 'behind', { cause: 'catching_up' }),
  nodeEntry('c.py:waits', 'file:c.py', 'undecided', { cause: 'undecided' }),
  scopeEntry('file:b.py', 'file', ROOT, ['b.py:mart', 'b.py:src'], { never_run: 1, ok: 1 }, { causes: causeCounts({ no_refresh_path: 1 }), not_fresh: 1, held_by: [heldBy('no_schedule:b.py:mart', 1, 1)] }),
  nodeEntry('b.py:mart', 'file:b.py', 'never_run', { cause: 'no_refresh_path' }),
  nodeEntry('b.py:src', 'file:b.py', 'ok'),
], [failureRoot('a.py:raw', 'TypeError: x', 3)]))

/** A segment's colours and how much of it each takes. */
const parts = (a: { start: number; end: number; bands: { part: string; start: number; end: number }[] }) =>
  a.bands.map((b) => [b.part, Number(((b.end - b.start) / (a.end - a.start)).toFixed(3))])

describe('the rollup, indexed', () => {
  it('reads every count off the entry: nodes and assets are both there', () => {
    const root = byFile.entries.get(ROOT)!
    expect([root.nodes, root.assets, root.not_fresh]).toEqual([7, 7, 6])
    expect(worstOf(root.health)).toBe('failing')
    expect(worstOf(counts())).toBe('ok')
    expect(healthSummary(root.health)).toBe('1 failing · 2 blocked · 1 behind')
    expect(healthSummary(root.health, 2)).toBe('1 failing · 2 blocked')
    expect(healthSummary(counts({ ok: 3 }))).toBe('')
  })

  it('finds the path to an entry, its leaves and the roots about them', () => {
    expect(treePath(byFile, 'a.py:clean').map((e) => e.id)).toEqual([ROOT, 'file:a.py', 'a.py:clean'])
    expect(treePath(byFile, 'nope')).toEqual([])
    expect(leafIds(byFile, 'file:c.py')).toEqual(['c.py:x', 'c.py:waits'])
    expect(leafIds(byFile, 'a.py:raw')).toEqual(['a.py:raw'])
    expect(nearestInTree(byFile, 'file:b.py')).toBe('file:b.py')
    expect(nearestInTree(byFile, 'group:gone')).toBe(ROOT)
    expect(nearestInTree(byFile, null)).toBe(ROOT)
    expect(rootsWithin(byFile, ROOT).map((r) => r.node)).toEqual(['a.py:raw'])
    expect(rootsWithin(byFile, 'file:a.py').map((r) => r.node)).toEqual(['a.py:raw'])
    expect(rootsWithin(byFile, 'file:b.py')).toEqual([])
  })

  it('says when a scope is expected fresh and how many nodes have no estimate, as served', () => {
    const root = byFile.entries.get(ROOT)!
    // 4m10s from `as_of` (14:57:00): rounded up to the unit shown, never sooner.
    expect(freshWithin(root, byFile.asOf)).toBe('fresh within about 5m')
    expect(freshWithin({ fresh_by: '2026-11-17T14:57:20Z' }, byFile.asOf)).toBe('fresh within about 20s')
    expect(freshWithin({ fresh_by: '2026-11-18T06:00:00Z' }, byFile.asOf)).toBe('fresh within about 16h')
    expect(freshWithin({ fresh_by: null }, byFile.asOf)).toBeNull()
    expect(noEstimate(root)).toBe('5 with no estimate')
    expect(noEstimate({ unbounded: 0 })).toBeNull()
  })

  it('collects what a root holds: the scopes that list it and the nodes whose row names it', () => {
    const rows = [
      { id: 'a.py:raw', root: { id: 'failure:a.py:raw' } },
      { id: 'a.py:clean', root: { id: 'failure:a.py:raw' } },
      { id: 'b.py:mart', root: { id: 'no_schedule:b.py:mart' } },
      { id: 'b.py:src', root: null },
    ]
    expect([...heldIds(byFile, rows, 'failure:a.py:raw')].sort()).toEqual(['a.py:clean', 'a.py:raw', 'file:a.py', 'root'])
    expect([...heldIds(byFile, rows, 'nothing:here')]).toEqual([])
  })
})

describe('the health map', () => {
  it('lays an entry out by its served weight, and holds no weight of its own', () => {
    // The fixture states what the server would ("Distance from fresh": 1 a fresh node, 2 one
    // that will be fresh without anyone acting or is undecided, 6 one that needs someone).
    const entry = (id: string) => byFile.entries.get(id)!
    expect([entry('b.py:src').weight, entry('c.py:x').weight, entry('c.py:waits').weight, entry('b.py:mart').weight, entry('a.py:raw').weight]).toEqual([1, 2, 2, 6, 6])
    expect([entry('file:a.py').weight, entry('file:c.py').weight, entry('file:b.py').weight, entry(ROOT).weight]).toEqual([18, 4, 7, 29])
    // A served weight the fixture's table would not give is drawn as served: half the turn.
    const served = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['file:a.py', 'file:b.py'], { ok: 2 }, { weight: 10 }),
      scopeEntry('file:a.py', 'file', ROOT, [], { ok: 1 }, { weight: 5 }),
      scopeEntry('file:b.py', 'file', ROOT, [], { ok: 1 }, { weight: 5 }),
    ]))
    expect(sunburst(served, ROOT, 1).map((a) => [a.id, a.end - a.start])).toEqual([['file:a.py', 0.5], ['file:b.py', 0.5]])
  })

  it('gives each entry the share of the turn its weight is, worst first, children inside their parent', () => {
    const arcs = sunburst(byFile, ROOT, 2)
    const first = arcs.filter((a) => a.depth === 1)
    expect(first.map((a) => a.id)).toEqual(['file:a.py', 'file:c.py', 'file:b.py'])
    expect(first[0]!.start).toBe(0)
    expect(first[0]!.end).toBeCloseTo(18 / 29)
    expect(first[1]!.end - first[1]!.start).toBeCloseTo(4 / 29)
    expect(first.at(-1)!.end).toBeCloseTo(1)
    const raw = arcs.find((a) => a.id === 'a.py:raw')!
    expect([raw.parent, raw.depth, raw.kind]).toEqual(['file:a.py', 2, 'node'])
    expect(raw.end - raw.start).toBeCloseTo(6 / 29)
    // Focused on a file, its nodes are the first ring.
    expect(sunburst(byFile, 'file:c.py', 1).map((a) => a.id)).toEqual(['c.py:x', 'c.py:waits'])
    expect(sunburst(byFile, 'nope')).toEqual([])
  })

  it('colours a segment by what is inside it, stacked by weight, never by its worst member', () => {
    const first = sunburst(byFile, ROOT, 1)
    // All of a.py is failing; c.py is half undecided, half catching up; b.py is 6 parts no
    // refresh path to 1 part fresh.
    expect(parts(first[0]!)).toEqual([['failing', 1]])
    expect(parts(first[1]!)).toEqual([['undecided', 0.5], ['catching_up', 0.5]])
    expect(parts(first[2]!)).toEqual([['no_refresh_path', 0.857], ['fresh', 0.143]])
    // The bands tile their segment.
    for (const a of first) {
      expect(a.bands[0]!.start).toBeCloseTo(a.start)
      expect(a.bands.at(-1)!.end).toBeCloseTo(a.end)
    }
    // A group with 13 of 100 nodes overdue is not one solid colour.
    const mostlyFine = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['group:g.py:g'], { behind: 13, ok: 87 }, { causes: causeCounts({ overdue: 13 }), not_fresh: 13 }),
      scopeEntry('group:g.py:g', 'group', ROOT, [], { behind: 13, ok: 87 }, { causes: causeCounts({ overdue: 13 }), not_fresh: 13 }),
    ]))
    expect(parts(sunburst(mostlyFine, ROOT, 1)[0]!)).toEqual([['overdue', 0.473], ['fresh', 0.527]])
    // A node is one colour: its cause, or fresh.
    const nodes = sunburst(byFile, 'file:b.py', 1)
    expect(nodes.map((a) => a.bands.map((b) => b.part))).toEqual([['no_refresh_path'], ['fresh']])
  })

  it('keeps a fully fresh group on the map, small beside one that is behind', () => {
    const tree = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['group:g.py:late', 'group:g.py:fine'], { behind: 20, ok: 20 }, { causes: causeCounts({ no_refresh_path: 20 }), not_fresh: 20 }),
      scopeEntry('group:g.py:late', 'group', ROOT, [], { behind: 20 }, { causes: causeCounts({ no_refresh_path: 20 }), not_fresh: 20 }),
      scopeEntry('group:g.py:fine', 'group', ROOT, [], { ok: 20 }),
    ]))
    const [late, fine] = sunburst(tree, ROOT, 1)
    expect(late!.end - late!.start).toBeCloseTo(120 / 140)
    expect(fine!.end - fine!.start).toBeCloseTo(20 / 140)
    expect(fine!.bands.map((b) => b.part)).toEqual(['fresh'])
  })

  it("draws a partitioned node's keys outside it: failed, not up to date, ok", () => {
    const keys = sunburst(byFile, ROOT, 3).filter((a) => a.kind === 'keys')
    expect(keys.map((a) => [a.id, a.depth, a.bands[0]!.part, a.label])).toEqual([
      ['a.py:raw::keys:failed', 3, 'failing', '100 keys failed'],
      ['a.py:raw::keys:ok', 3, 'fresh', '20 keys ok'],
    ])
    const raw = sunburst(byFile, ROOT, 3).find((a) => a.id === 'a.py:raw')!
    expect(keys[0]!.start).toBeCloseTo(raw.start)
    expect(keys[1]!.end).toBeCloseTo(raw.end)
    expect(keys[0]!.keys).toEqual({ count: 100, of: 120 })
    // A few failed keys among many stay visible, and the bands still fill the node.
    const big = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['k.py:k'], { failing: 1 }, { causes: causeCounts({ failing: 1 }), not_fresh: 1 }),
      nodeEntry('k.py:k', ROOT, 'failing', { cause: 'failing', keys: { failed: 24, behind: 0, ok: 4976 } }),
    ]))
    const bands = sunburst(big, 'k.py:k', 1, 0.02)
    expect(bands[0]!.end - bands[0]!.start).toBeCloseTo(0.01)
    expect(bands.at(-1)!.end).toBeCloseTo(1)
    // Keys not up to date take the node's own colour: why it is not fresh.
    const waiting = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['k.py:k'], { behind: 1 }, { causes: causeCounts({ no_refresh_path: 1 }), not_fresh: 1 }),
      nodeEntry('k.py:k', ROOT, 'behind', { cause: 'no_refresh_path', keys: { failed: 0, behind: 3, ok: 1 } }),
    ]))
    expect(sunburst(waiting, 'k.py:k').map((a) => [a.label, a.bands[0]!.part])).toEqual([['3 keys not up to date', 'no_refresh_path'], ['1 key ok', 'fresh']])
    // Not past the last ring, and not under a node too thin to hold them.
    expect(sunburst(byFile, ROOT, 2).some((a) => a.kind === 'keys')).toBe(false)
    expect(sunburst(byFile, ROOT, 3, 0.5).some((a) => a.kind === 'keys')).toBe(false)
    // A node at the centre shows its keys as the first ring.
    expect(sunburst(byFile, 'a.py:raw').map((a) => a.id)).toEqual(['a.py:raw::keys:failed', 'a.py:raw::keys:ok'])
  })

  it('draws members too thin to show one by one as one segment, coloured by what is inside them', () => {
    const pad = (i: number) => String(i).padStart(2, '0')
    const ok = Array.from({ length: 60 }, (_, i) => nodeEntry(`m.py:n${pad(i)}`, 'file:m.py', 'ok'))
    const late = Array.from({ length: 30 }, (_, i) => nodeEntry(`m.py:s${pad(i)}`, 'file:m.py', 'behind', { cause: 'catching_up' }))
    const broken = nodeEntry('m.py:broken', 'file:m.py', 'failing', { cause: 'failing' })
    const members = [broken, ...late, ...ok]
    const sums = { causes: causeCounts({ failing: 1, catching_up: 30 }), not_fresh: 31 }
    const tree = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['file:m.py'], { failing: 1, behind: 30, ok: 60 }, sums),
      scopeEntry('file:m.py', 'file', ROOT, members.map((m) => m.id), { failing: 1, behind: 30, ok: 60 }, sums),
      ...members,
    ]))
    // Weights: 6 failing, 60 catching up, 60 fresh. The failing node alone is wide enough to draw.
    const arcs = sunburst(tree, 'file:m.py', 1, 0.02)
    expect(arcs.map((a) => [a.id, a.kind, a.label])).toEqual([
      ['m.py:broken', 'node', 'broken'],
      ['file:m.py::rest', 'rest', '90 smaller'],
    ])
    expect(parts(arcs[1]!)).toEqual([['catching_up', 0.5], ['fresh', 0.5]])
    expect(arcs[1]!.members).toHaveLength(90)
    // The ring is whole: the segments tile the turn.
    expect(arcs.reduce((sum, a) => sum + (a.end - a.start), 0)).toBeCloseTo(1)
  })
})

describe('arc geometry', () => {
  const angles = (d: string) => [...d.matchAll(/(-?\d+\.\d+) (-?\d+\.\d+)/g)].map((m) => Math.atan2(Number(m[1]), -Number(m[2])))
  it('parts neighbours by an even gap at every radius', () => {
    const plain = angles(arcPath(0, 0.25, 50, 100))
    const gapped = angles(arcPath(0, 0.25, 50, 100, 2))
    // The outer edge moves in by 1 px at radius 100, the inner by 1 px at radius 50: twice the angle.
    expect(gapped[0]! - plain[0]!).toBeCloseTo(1 / 100)
    expect(gapped[3]! - plain[3]!).toBeCloseTo(1 / 50)
    // A sector thinner than its gaps keeps a hairline instead of turning inside out.
    expect(() => arcPath(0.1, 0.1001, 50, 100, 4)).not.toThrow()
    expect(arcPath(0, 1, 50, 100, 2)).toContain('A 100 100 0 1 1')
    // A colour band inside a segment keeps the gap only on the segment's own sides.
    const inner = angles(arcPath(0, 0.25, 50, 100, 2, [false, true]))
    expect(inner[0]!).toBeCloseTo(plain[0]!)
    expect(inner[1]!).toBeCloseTo(gapped[1]!)
  })
})

describe('ring radii', () => {
  it('thins each ring out, fills the space, and never makes one level a thick band', () => {
    const shape = { firstMax: 0.27, decay: 0.8, gap: 1 }
    const rings = ringRadii(4, 30, 150, shape)
    expect(rings).toHaveLength(4)
    const widths = rings.map(([a, b]) => b - a)
    for (let k = 1; k < 4; k++) expect(widths[k]! / widths[k - 1]!).toBeCloseTo(0.8)
    expect(rings[0]![0]).toBe(31)
    expect(rings[3]![1]).toBeCloseTo(150)
    // One level: as wide as the cap allows, not the whole span.
    const one = ringRadii(1, 30, 150, shape)
    expect(one[0]![1] - one[0]![0]).toBeCloseTo(150 * 0.27)
    expect(ringRadii(0, 30, 150, shape)).toEqual([])
  })
})
