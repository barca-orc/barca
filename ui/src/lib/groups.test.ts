import { describe, expect, it } from 'vitest'
import {
  MODELING_GROUPS as tree,
  PREVIEW_FAILURE,
  groupHealth,
  groupLeaves,
  groupOutput,
  groupPath,
  projectGroups,
  memberHealth,
} from './groups'
import type { AssetSummary, NodeState } from './types'

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
    next_run: null,
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
    expect(groupOutput('group:training_data', tree)).toBe(
      'modeling.py:data__train_rows',
    )
  })

  it('propagates a failed check through all ancestors while its model output stays cached', () => {
    const leaves = tree.roots.flatMap((id) => groupLeaves(id, tree))
    const states = new Map(leaves.map((id) => [id, state(id)]))
    const failure = states.get(PREVIEW_FAILURE)!
    failure.last_materialization!.status = 'failed'
    for (const id of [
      'group:fold_03/fitting',
      'group:fold_03',
      'group:cross_validation',
    ]) {
      expect(groupHealth(groupLeaves(id, tree), states).status).toBe('failed')
      expect(memberHealth(states.get(groupOutput(id, tree))).status).toBe(
        'success',
      )
    }
    expect(groupHealth(groupLeaves('group:fold_02', tree), states).status).toBe(
      'success',
    )
    expect(groupPath('group:fold_03/fitting', tree).map((g) => g.name)).toEqual(
      ['Cross-validation', 'Fold 3', 'Model fitting'],
    )
  })

  it('treats successful always-run checks as healthy, and missing state as unknown', () => {
    const ids = groupLeaves('group:final_training', tree)
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
    const assets: AssetSummary[] = [
      {
        id: 'modeling.py:data__raw_rows',
        kind: 'asset',
        inputs: [],
        freshness: { type: 'Always' },
        env: [],
      },
      {
        id: 'modeling.py:data__train_rows',
        kind: 'asset',
        inputs: ['modeling.py:data__raw_rows'],
        freshness: { type: 'Always' },
        env: [],
      },
      {
        id: 'modeling.py:cv__fold_01__train_rows',
        kind: 'asset',
        inputs: ['modeling.py:data__raw_rows', 'modeling.py:data__train_rows'],
        freshness: { type: 'Always' },
        env: [],
      },
    ]
    const original = structuredClone(assets)
    const root = projectGroups(assets, tree, null)
    expect(
      root.entries.find((e) => e.id === 'group:cross_validation')?.inputs,
    ).toEqual(['group:training_data'])
    const fold = projectGroups(assets, tree, 'group:fold_01')
    expect(fold.external).toContain('modeling.py:data__raw_rows')
    expect(assets).toEqual(original)
  })
})
