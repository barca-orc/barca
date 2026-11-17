import { describe, expect, it } from 'vitest'
import { activityNow, groupOfNodes } from './runLive'
import type { ActiveStep } from './types'

describe('activityNow', () => {
  const active: ActiveStep[] = [
    { node_id: 'b.py:keys', step: 'running', partitions: { running: 1880, done: 3120, total: 5000 } },
    { node_id: 'b.py:one', step: 'running', partitions: null },
    { node_id: 'c.py:later', step: 'queued', partitions: null },
  ]
  const groups = [
    { id: 'g:billing', name: 'billing', description: '', members: ['b.py:keys', 'b.py:one', 'c.py:later'], output: '' },
    { id: 'g:billing.raw', name: 'billing.raw', description: '', members: ['b.py:keys'], output: '' },
  ]

  it('lists what runs now, furthest from done first, and by group', () => {
    const now = activityNow(active, groups)
    expect(now.running.map((r) => r.nodeId)).toEqual(['b.py:keys', 'b.py:one'])
    expect(now.running[0]!.keys).toBe('3,120 of 5,000 keys')
    expect(now.queued).toBe(1)
    expect(now.keys).toEqual({ done: 3120, total: 5000 })
    expect(now.groups).toEqual([{ group: 'billing', running: 1, queued: 1 }, { group: 'billing.raw', running: 1, queued: 0 }])
  })

  it('has no keys without a partitioned node in flight', () => {
    expect(activityNow([], []).keys).toBeNull()
  })

  it('takes the innermost group of a node', () => {
    const g = groupOfNodes([
      { id: 'a', name: 'all', description: '', members: ['x', 'y'], output: '' },
      { id: 'b', name: 'inner', description: '', members: ['x'], output: '' },
    ])
    expect(g.get('x')).toBe('inner')
    expect(g.get('y')).toBe('all')
  })
})
