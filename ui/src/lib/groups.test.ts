import metadata from '../../e2e/group-metadata.json'
import { describe, expect, it } from 'vitest'
import {
  compareEntries,
  distinctNames,
  parseEntrySort,
  groupTree,
  PREVIEW_FAILURE,
  groupHealth,
  groupLeaves,
  groupOutput,
  groupPath,
  projectGroups,
  memberHealth,
} from './groups'
const tree = groupTree(metadata)

import type { ListNode, NodeState } from './types'

function state(id: string): NodeState {
  const task = id.includes(':validate__')
  return {
    id,
    name: id.split(':')[1]!,
    kind: task ? 'task' : 'asset',
    inputs: [],
    partitioned: false,
    cache: {
      state: task ? 'always_runs' : 'cached',
      reason: task ? 'task' : 'materialized',
      detail: 'Demo state',
    },
    last_materialization: {
      status: 'success',
      created_at: '2026-10-09 00:00:00',
      elapsed_seconds: null,
      run_hash: null,
      artifact: null,
      format: null,
      size_bytes: null,
    },
    durations: null,
    next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0,
    running: null,
    env: [],
    shape: null,
  }
}

describe('organizational groups', () => {
  it('owns every modeling node exactly once and resolves outputs inside their groups', () => {
    const leaves = tree.roots.flatMap((id) => groupLeaves(id, tree))
    expect(leaves).toHaveLength(152)
    expect(new Set(leaves).size).toBe(152)
    for (const group of tree.groups.values())
      expect(groupLeaves(group.id, tree)).toContain(groupOutput(group.id, tree))
    expect(groupOutput('group:modeling.py:training_data', tree)).toBe(
      'modeling.py:data__train_rows',
    )
  })

  it('propagates a failed check through all ancestors while its model output stays cached', () => {
    const leaves = tree.roots.flatMap((id) => groupLeaves(id, tree))
    const states = new Map(leaves.map((id) => [id, state(id)]))
    const failure = states.get(PREVIEW_FAILURE)!
    failure.last_materialization!.status = 'failed'
    for (const id of [
      'group:modeling.py:fold_03__fitting',
      'group:modeling.py:fold_03',
      'group:modeling.py:cross_validation',
    ]) {
      expect(groupHealth(groupLeaves(id, tree), states).status).toBe('failed')
      expect(memberHealth(states.get(groupOutput(id, tree))).status).toBe(
        'success',
      )
    }
    expect(groupHealth(groupLeaves('group:modeling.py:fold_02', tree), states).status).toBe(
      'success',
    )
    expect(groupPath('group:modeling.py:fold_03__fitting', tree).map((g) => g.name)).toEqual(
      ['Cross-validation', 'Fold 3', 'Model fitting'],
    )
  })

  it('treats successful always-run checks as healthy, and missing state as unknown', () => {
    const ids = groupLeaves('group:modeling.py:final_training', tree)
    const states = new Map(ids.map((id) => [id, state(id)]))
    expect(groupHealth(ids, states).status).toBe('success')
    states.delete(ids[0]!)
    expect(groupHealth(ids, states).status).toBe('skipped')
    states.set(ids[0]!, {
      ...state(ids[0]!),
      cache: { state: 'stale', reason: 'changed', detail: 'Changed' },
    })
    expect(groupHealth(ids, states).status).toBe('warning')
  })

  it('preserves dependencies crossing a collapsed boundary even through a non-output member', () => {
    const assets: ListNode[] = [
      {
        id: 'modeling.py:data__raw_rows',
        kind: 'asset',
        inputs: [],
        freshness: 'always',
        env: [],
      },
      {
        id: 'modeling.py:data__train_rows',
        kind: 'asset',
        inputs: ['modeling.py:data__raw_rows'],
        freshness: 'always',
        env: [],
      },
      {
        id: 'modeling.py:cv__fold_01__train_rows',
        kind: 'asset',
        inputs: ['modeling.py:data__raw_rows', 'modeling.py:data__train_rows'],
        freshness: 'always',
        env: [],
      },
    ]
    const original = structuredClone(assets)
    const root = projectGroups(assets, tree, null)
    expect(
      root.entries.find((e) => e.id === 'group:modeling.py:cross_validation')?.inputs,
    ).toEqual(['group:modeling.py:training_data'])
    const fold = projectGroups(assets, tree, 'group:modeling.py:fold_01')
    expect(fold.external).toContain('modeling.py:data__raw_rows')
    expect(assets).toEqual(original)
  })
})

describe('table names', () => {
  it('shows the part that distinguishes a row', () => {
    const names = distinctNames(['b.py:billing__raw__invoices_00', 'b.py:check__billing__raw__invoices_00'])
    expect([...names.values()]).toEqual(['invoices_00', 'check__invoices_00'])
  })
  it('never repeats a label', () => {
    const ids = ['b.py:raw__x', 'b.py:clean__x', 'a.py:raw__x', 'b.py:y']
    const names = distinctNames(ids)
    expect(names.get('b.py:y')).toBe('y')
    expect(new Set(names.values()).size).toBe(ids.length)
  })
})

describe('entry order', () => {
  const rank = (name: string, failed = 0, stale = 0, lastSuccessAt: string | null = null) => ({ name, failed, stale, lastSuccessAt })
  it('is worst first, then by name', () => {
    const rows = [rank('a_10'), rank('a_2'), rank('z', 0, 3), rank('y', 1)]
    expect(rows.sort((a, b) => compareEntries('worst', a, b)).map((r) => r.name)).toEqual(['y', 'z', 'a_2', 'a_10'])
  })
  it('reads the sort from the URL', () => {
    expect(parseEntrySort('name')).toBe('name')
    expect(parseEntrySort('bogus')).toBe('worst')
  })
})
