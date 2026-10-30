import { describe, expect, it } from 'vitest'
import { groupRunSteps, runNodeLine } from './runsPage'
import { failedKeysLabel, keyBreakdown, severityOf } from './assetTable'
import type { NodeState, RunStep } from './types'

const step = (node_id: string, status: string) => ({ node_id, status }) as RunStep
const node = (partitions: NodeState['partitions']) =>
  ({ partitions, last_materialization: { status: 'success' }, cache: { state: 'partial' } }) as unknown as NodeState

describe('groupRunSteps', () => {
  it('counts from the summary, however few keys the page lists, with failed keys first', () => {
    const summary = { node_id: 'p.py:a', keys: 3077, failed: 1, elapsed_seconds: 2, calls: 7 }
    const [group] = groupRunSteps([step('p.py:a[k=1]', 'success'), step('p.py:a[k=2]', 'failed'), step('p.py:b', 'success')], [summary])
    expect(group!.summary.keys).toBe(3077)
    expect(group!.steps.map((s) => s.node_id)).toEqual(['p.py:a[k=2]', 'p.py:a[k=1]'])
  })

  it('says keys and calls in a line, and only what happened', () => {
    expect(runNodeLine({ node_id: 'a', keys: 3077, failed: 40, elapsed_seconds: 1, calls: 7 })).toBe('3,077 keys · 40 failed · 7 calls')
    expect(runNodeLine({ node_id: 'a', keys: 1, failed: 0, elapsed_seconds: 1, calls: 0 })).toBe('1 key')
  })
})

describe('a partitioned node with failed keys', () => {
  const partial = { total: 3077, cached: 3037, missing: 40, missing_keys: [], failed: 40 }

  it('is failed although the key its last materialization names succeeded', () => {
    expect(severityOf(node(partial))).toBe('failed')
    expect(failedKeysLabel(node(partial))).toBe('40 of 3,077 keys failed')
  })

  it('stays partial when its missing keys are only stale', () => {
    expect(severityOf(node({ ...partial, failed: 0 }))).toBe('partial')
    expect(failedKeysLabel(node({ ...partial, failed: 0 }))).toBeNull()
  })

  it('splits its keys into parts that add up to the total', () => {
    expect(keyBreakdown({ ...partial, cached: 3000, missing: 77 })).toEqual({ cached: 3000, failed: 40, stale: 37, total: 3077 })
  })
})
