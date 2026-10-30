import { describe, expect, it } from 'vitest'
import { runButton, runTarget } from './runTarget'

const assets = [
  { id: 'billing.py:a', kind: 'asset' as const },
  { id: 'billing.py:b', kind: 'sensor' as const },
  { id: 'billing.py:check__a', kind: 'task' as const },
  { id: 'crm.py:c', kind: 'asset' as const },
]
const none = { node: null, group: null, pipeline: null }

describe('runTarget', () => {
  it('prefers the selected node', () => {
    expect(runTarget({ ...none, node: 'a', group: 'group:g', pipeline: 'billing.py' }, assets)).toEqual({ kind: 'node', name: 'a' })
  })
  it('counts what a get of the scope runs: no tasks', () => {
    expect(runTarget({ ...none, pipeline: 'billing.py' }, assets)).toEqual({ kind: 'pipeline', file: 'billing.py', nodes: 2 })
    expect(runTarget(none, assets)).toEqual({ kind: 'all', nodes: 3 })
  })
  it('has nothing to run in an open group, or before assets load', () => {
    expect(runTarget({ ...none, group: 'group:g' }, assets).kind).toBe('none')
    expect(runTarget(none, []).kind).toBe('none')
  })
})

describe('runButton', () => {
  it('names its target and size', () => {
    expect(runButton({ kind: 'node', name: 'invoices_00' }).label).toBe('Run invoices_00')
    expect(runButton({ kind: 'pipeline', file: 'x/billing.py', nodes: 506 }).label).toBe('Run billing.py (506 nodes)')
    expect(runButton({ kind: 'pipeline', file: 'a.py', nodes: 1 }).label).toBe('Run a.py (1 node)')
  })
  it('asks first only for the whole project', () => {
    expect(runButton({ kind: 'all', nodes: 8070 })).toMatchObject({ label: 'Run all 8,070 nodes', confirm: { action: 'Run all 8,070 nodes' } })
    expect(runButton({ kind: 'pipeline', file: 'a.py', nodes: 2 }).confirm).toBeNull()
    expect(runButton({ kind: 'none', hint: 'h' })).toEqual({ label: 'Run', title: 'h', confirm: null })
  })
})
