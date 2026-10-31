import { describe, expect, it } from 'vitest'
import { groupRunSteps, runNodeIdle, runNodeLine, runNodeOutcome, runNodeTime, runTargetLines, splitRunSteps } from './runsPage'
import { failedKeysLabel, formatSeconds, keyBreakdown, severityOf } from './assetTable'
import type { NodeState, RunNodeSummary, RunStep } from './types'

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

  it('splits the listed keys into failed and successful', () => {
    const { failed, ok } = splitRunSteps([step('a[k=1]', 'success'), step('a[k=2]', 'failed'), step('a[k=3]', 'success')])
    expect(failed.map((s) => s.node_id)).toEqual(['a[k=2]'])
    expect(ok).toHaveLength(2)
  })
})

describe('a run\'s planned nodes', () => {
  const idle = (node_id: string, status: string, blocked_by?: string): RunNodeSummary =>
    ({ node_id, keys: 0, failed: 0, elapsed_seconds: 0, calls: 0, status, blocked_by })
  const forecast: RunNodeSummary = { node_id: 'p.py:demand_forecast', keys: 3077, failed: 7, elapsed_seconds: 2.2, calls: 5, status: 'ran' }
  const nodes = [
    idle('p.py:retailer_accounts', 'cached'),
    forecast,
    idle('p.py:promo_plan', 'skipped', 'p.py:demand_forecast'),
    idle('p.py:publish_plan', 'skipped', 'p.py:demand_forecast'),
  ]

  it('labels cached and skipped nodes, and why a node was skipped', () => {
    expect(runNodeOutcome(nodes[0]!)).toEqual({ kind: 'skipped', label: 'cached', detail: null })
    expect(runNodeOutcome(nodes[2]!)).toEqual({ kind: 'skipped', label: 'skipped', detail: 'upstream demand_forecast failed' })
    expect(runNodeOutcome(idle('p.py:late', 'skipped'))).toMatchObject({ label: 'skipped', detail: 'the run stopped before it' })
    expect(runNodeOutcome(forecast)).toEqual({ kind: 'failed', label: '3,077 keys · 7 failed · 5 calls', detail: null })
    expect(nodes.map(runNodeIdle)).toEqual([true, false, true, true])
  })

  it('labels a batched node\'s time as summed over its calls', () => {
    expect(runNodeTime(forecast, formatSeconds)).toMatchObject({ text: '2.2s over 5 calls', title: expect.stringContaining('summed over 5 batched calls') })
    expect(runNodeTime({ ...forecast, calls: 0, keys: 1, elapsed_seconds: 0.5 }, formatSeconds)).toEqual({ text: '0.5s', title: 'Compute time' })
    expect(runNodeTime(nodes[0]!, formatSeconds)).toBeNull()
  })

  it('states what became of the target at the top', () => {
    expect(runTargetLines({ target: 'publish_plan', job: 'p.py:publish_plan' }, nodes)).toEqual(['publish_plan did not run: upstream demand_forecast failed 7 keys'])
    expect(runTargetLines({ target: 'demand_forecast', job: 'p.py:demand_forecast' }, nodes)).toEqual(['demand_forecast failed: 7 of 3,077 keys'])
    expect(runTargetLines({ target: 'retailer_accounts', job: null }, nodes)).toEqual(['retailer_accounts was cached: nothing to do'])
    expect(runTargetLines({ target: 'late', job: 'p.py:late' }, [...nodes, idle('p.py:late', 'skipped')])).toEqual(['late did not run: the run stopped before it'])
    expect(runTargetLines({ target: 'publish_plan', job: 'p.py:publish_plan' }, [forecast].map((n) => ({ ...n, status: undefined })))).toEqual([])
    expect(runTargetLines({ target: null, job: 'all' }, nodes)).toEqual([])
  })

  it('lists every planned node, the idle ones with no keys', () => {
    const groups = groupRunSteps([step('p.py:demand_forecast[account=1]', 'failed')], nodes)
    expect(groups.map((g) => [g.summary.node_id, g.steps.length])).toEqual([['p.py:retailer_accounts', 0], ['p.py:demand_forecast', 1], ['p.py:promo_plan', 0], ['p.py:publish_plan', 0]])
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
