import { describe, expect, it } from 'vitest'
import { ROOT, arcPath, healthSummary, indexRollup, leafIds, mapUnit, nearestInTree, ringRadii, rootsWithin, sunburst, treePath, worstOf } from './rollupView'
import { counts, failureRoot, nodeEntry, rollup, scopeEntry } from './testdata/rollup'

// a.py: raw (failing, 100 of 120 keys failed) → clean, report (blocked by it)
// b.py: src (ok) → mart (never run); c.py: x (behind), waits (undecided)
const byFile = indexRollup(rollup([
  scopeEntry(ROOT, 'root', null, ['file:a.py', 'file:c.py', 'file:b.py'], { failing: 1, blocked: 2, behind: 1, undecided: 1, never_run: 1, ok: 1 }),
  scopeEntry('file:a.py', 'file', ROOT, ['a.py:raw', 'a.py:clean', 'a.py:report'], { failing: 1, blocked: 2 }),
  nodeEntry('a.py:raw', 'file:a.py', 'failing', { keys: { failed: 100, behind: 0, ok: 20 } }),
  nodeEntry('a.py:clean', 'file:a.py', 'blocked'),
  nodeEntry('a.py:report', 'file:a.py', 'blocked'),
  scopeEntry('file:c.py', 'file', ROOT, ['c.py:x', 'c.py:waits'], { behind: 1, undecided: 1 }),
  nodeEntry('c.py:x', 'file:c.py', 'behind'),
  nodeEntry('c.py:waits', 'file:c.py', 'undecided'),
  scopeEntry('file:b.py', 'file', ROOT, ['b.py:mart', 'b.py:src'], { never_run: 1, ok: 1 }),
  nodeEntry('b.py:mart', 'file:b.py', 'never_run'),
  nodeEntry('b.py:src', 'file:b.py', 'ok'),
], [failureRoot('a.py:raw', 'TypeError: x', 3)]))

describe('the rollup, indexed', () => {
  it('reads every count off the entry: nodes and assets are both there', () => {
    const root = byFile.entries.get(ROOT)!
    expect([root.nodes, root.assets]).toEqual([7, 7])
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
})

describe('the health map', () => {
  it('gives each entry the share of the turn its assets are, worst first, children inside their parent', () => {
    const arcs = sunburst(byFile, ROOT, 2)
    const first = arcs.filter((a) => a.depth === 1)
    expect(first.map((a) => [a.id, a.size, a.health])).toEqual([['file:a.py', 3, 'failing'], ['file:c.py', 2, 'behind'], ['file:b.py', 2, 'never_run']])
    expect(first[0]!.start).toBe(0)
    expect(first[0]!.end).toBeCloseTo(3 / 7)
    expect(first.at(-1)!.end).toBeCloseTo(1)
    const raw = arcs.find((a) => a.id === 'a.py:raw')!
    expect([raw.parent, raw.depth, raw.kind]).toEqual(['file:a.py', 2, 'node'])
    expect(raw.end - raw.start).toBeCloseTo(1 / 7)
    // Focused on a file, its nodes are the first ring.
    expect(sunburst(byFile, 'file:c.py', 1).map((a) => a.id)).toEqual(['c.py:x', 'c.py:waits'])
    expect(sunburst(byFile, 'nope')).toEqual([])
  })

  it('sizes by assets: a task colours its group but takes no room, unless there is no asset at all', () => {
    const mixed = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['file:a.py'], { failing: 1, ok: 1 }, { assets: 1, asset_health: counts({ ok: 1 }) }),
      scopeEntry('file:a.py', 'file', ROOT, ['a.py:job', 'a.py:data'], { failing: 1, ok: 1 }, { assets: 1, asset_health: counts({ ok: 1 }) }),
      nodeEntry('a.py:job', 'file:a.py', 'failing', { task: true }),
      nodeEntry('a.py:data', 'file:a.py', 'ok'),
    ]))
    expect(mapUnit(mixed.entries.get(ROOT)!)).toBe('assets')
    // The file is red (a task in it fails) and one asset wide; the task itself is not drawn.
    expect(sunburst(mixed, ROOT).map((a) => [a.id, a.size, a.health])).toEqual([['file:a.py', 1, 'failing'], ['a.py:data', 1, 'ok']])
    const tasks = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['file:a.py'], { ok: 1 }, { assets: 0, asset_health: counts() }),
      scopeEntry('file:a.py', 'file', ROOT, ['a.py:job'], { ok: 1 }, { assets: 0, asset_health: counts() }),
      nodeEntry('a.py:job', 'file:a.py', 'ok', { task: true }),
    ]))
    expect(mapUnit(tasks.entries.get(ROOT)!)).toBe('nodes')
    expect(sunburst(tasks, ROOT).map((a) => [a.id, a.size])).toEqual([['file:a.py', 1], ['a.py:job', 1]])
  })

  it("draws a partitioned node's keys outside it: failed, not up to date, ok", () => {
    const keys = sunburst(byFile, ROOT, 3).filter((a) => a.kind === 'keys')
    expect(keys.map((a) => [a.id, a.depth, a.health, a.label])).toEqual([
      ['a.py:raw::keys:failed', 3, 'failing', '100 keys failed'],
      ['a.py:raw::keys:ok', 3, 'ok', '20 keys ok'],
    ])
    const raw = sunburst(byFile, ROOT, 3).find((a) => a.id === 'a.py:raw')!
    expect(keys[0]!.start).toBeCloseTo(raw.start)
    expect(keys[1]!.end).toBeCloseTo(raw.end)
    expect(keys[0]!.keys).toEqual({ count: 100, of: 120 })
    // A few failed keys among many stay visible, and the bands still fill the node.
    const big = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['k.py:k'], { failing: 1 }),
      nodeEntry('k.py:k', ROOT, 'failing', { keys: { failed: 24, behind: 0, ok: 4976 } }),
    ]))
    const bands = sunburst(big, 'k.py:k', 1, 0.02)
    expect(bands[0]!.end - bands[0]!.start).toBeCloseTo(0.01)
    expect(bands.at(-1)!.end).toBeCloseTo(1)
    // Keys not up to date take the node's own word.
    const waiting = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['k.py:k'], { blocked: 1 }),
      nodeEntry('k.py:k', ROOT, 'blocked', { keys: { failed: 0, behind: 3, ok: 1 } }),
    ]))
    expect(sunburst(waiting, 'k.py:k').map((a) => [a.label, a.health])).toEqual([['3 keys not up to date', 'blocked'], ['1 key ok', 'ok']])
    // Not past the last ring, and not under a node too thin to hold them.
    expect(sunburst(byFile, ROOT, 2).some((a) => a.kind === 'keys')).toBe(false)
    expect(sunburst(byFile, ROOT, 3, 0.5).some((a) => a.kind === 'keys')).toBe(false)
    // A node at the centre shows its keys as the first ring.
    expect(sunburst(byFile, 'a.py:raw').map((a) => a.id)).toEqual(['a.py:raw::keys:failed', 'a.py:raw::keys:ok'])
  })

  it('draws members too thin to show one by one as one band per colour, so a ring never frays', () => {
    const pad = (i: number) => String(i).padStart(2, '0')
    const ok = Array.from({ length: 60 }, (_, i) => nodeEntry(`m.py:n${pad(i)}`, 'file:m.py', 'ok'))
    const late = Array.from({ length: 30 }, (_, i) => nodeEntry(`m.py:s${pad(i)}`, 'file:m.py', 'behind'))
    const broken = nodeEntry('m.py:broken', 'file:m.py', 'failing')
    const members = [broken, ...late, ...ok]
    const tree = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['file:m.py'], { failing: 1, behind: 30, ok: 60 }),
      scopeEntry('file:m.py', 'file', ROOT, members.map((m) => m.id), { failing: 1, behind: 30, ok: 60 }),
      ...members,
    ]))
    const arcs = sunburst(tree, 'file:m.py', 1, 0.02)
    expect(arcs.map((a) => [a.id, a.kind, a.label, a.size, a.health])).toEqual([
      ['file:m.py::rest:failing', 'rest', '91 smaller', 1, 'failing'],
      ['file:m.py::rest:attention', 'rest', '91 smaller', 30, 'behind'],
      ['file:m.py::rest:ok', 'rest', '91 smaller', 60, 'ok'],
    ])
    // The ring is whole: the segments tile the turn.
    expect(arcs.reduce((sum, a) => sum + (a.end - a.start), 0)).toBeCloseTo(1)
  })

  it("sizes the thin members' bands by what is inside them, so mostly-fine small groups read mostly ok", () => {
    // 20 groups of 5: each has one node behind, so each group alone is `behind`.
    const groups = Array.from({ length: 20 }, (_, g) => scopeEntry(`group:g.py:g${g}`, 'group', 'group:g.py:all', [], { behind: 1, ok: 4 }))
    const tree = indexRollup(rollup([
      scopeEntry(ROOT, 'root', null, ['group:g.py:all'], { behind: 20, ok: 80 }),
      scopeEntry('group:g.py:all', 'group', ROOT, groups.map((g) => g.id), { behind: 20, ok: 80 }),
      ...groups,
    ]))
    const bands = sunburst(tree, 'group:g.py:all', 1, 0.1)
    expect(bands.map((a) => [a.id, a.health, a.size, a.label])).toEqual([
      ['group:g.py:all::rest:attention', 'behind', 20, '20 smaller'],
      ['group:g.py:all::rest:ok', 'ok', 80, '20 smaller'],
    ])
    expect(bands[1]!.end - bands[1]!.start).toBeCloseTo(0.8)
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
