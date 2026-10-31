import metadata from '../../e2e/group-metadata.json'
import { describe, expect, it } from 'vitest'
import type { GraphEntry } from './graph'
import {
  compareEntries,
  contentsSummary,
  lastRunDuration,
  distinctNames,
  plural,
  soleGroup,
  withPorts,
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
        env: [], file: 'p.py', line: 1,
      },
      {
        id: 'modeling.py:data__train_rows',
        kind: 'asset',
        inputs: ['modeling.py:data__raw_rows'],
        freshness: 'always',
        env: [], file: 'p.py', line: 1,
      },
      {
        id: 'modeling.py:cv__fold_01__train_rows',
        kind: 'asset',
        inputs: ['modeling.py:data__raw_rows', 'modeling.py:data__train_rows'],
        freshness: 'always',
        env: [], file: 'p.py', line: 1,
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

describe('soleGroup', () => {
  const node = (id: string): ListNode => ({ id, kind: 'asset', freshness: 'always', env: [], file: 'p.py', line: 1, inputs: [] })
  const tree = groupTree([{ id: 'group:p.py:g', name: 'G', description: '', members: ['p.py:a', 'p.py:b'], output: 'p.py:b' }])
  it('is the group that holds every node', () => {
    expect(soleGroup([node('p.py:a'), node('p.py:b')], tree)).toBe('group:p.py:g')
  })
  it('is none when a node sits outside it', () => {
    expect(soleGroup([node('p.py:a'), node('p.py:c')], tree)).toBeNull()
    expect(soleGroup([], tree)).toBeNull()
  })
})

describe('plural', () => {
  it('says 1 group, not 1 groups', () => {
    expect(plural(1, 'group')).toBe('1 group')
    expect(plural(1200, 'item')).toBe('1,200 items')
  })
})

describe('withPorts', () => {
  const members: GraphEntry[] = [{ id: 'a', kind: 'asset', inputs: [] }, { id: 'b', kind: 'asset', inputs: [] }]
  it('feeds each consumer from a port of its own input', () => {
    const out = withPorts(members, new Map([['q:x', ['a']]]), () => 'q')
    expect(out.map((e) => e.id)).toEqual(['q:x', 'a', 'b'])
    expect(out[0]).toMatchObject({ kind: 'port', name: 'x', loose: false })
    expect(out[1]!.inputs).toEqual(['q:x'])
  })
  it('merges many inputs by where they come from', () => {
    const external = new Map(Array.from({ length: 11 }, (_, i) => [`q:x${i}`, ['a', 'b']] as [string, string[]]))
    const out = withPorts(members, external, () => 'Staging')
    expect(out).toHaveLength(3)
    expect(out[0]).toMatchObject({ id: 'port:Staging', name: 'Staging · 11', loose: true })
    expect(out[2]!.inputs).toEqual(['port:Staging'])
  })
})

describe('contentsSummary', () => {
  const node = (id: string, kind: ListNode['kind']): Pick<ListNode, 'id' | 'kind'> => ({ id, kind })

  it('names what a group holds and leaves out what it has none of', () => {
    expect(contentsSummary([
      node('p.py:a', 'asset'), node('p.py:b', 'asset'), node('p.py:c', 'asset'), node('p.py:validate_publication', 'task'),
    ])).toBe('3 assets · 1 task')
    expect(contentsSummary([node('p.py:a', 'asset'), node('p.py:check__a', 'task'), node('p.py:s', 'sensor')])).toBe('1 asset · 1 check · 1 sensor')
    expect(contentsSummary([])).toBe('0 assets')
  })
})

describe('lastRunDuration', () => {
  const base = state('modeling.py:data__raw_rows')
  const run = { run_id: 'd969', elapsed_seconds: 2.25, keys: 3077, calls: 5, failed: 0 }

  it('adds a batched node up over its keys and calls, and says so', () => {
    expect(lastRunDuration({ ...base, last_run: run })).toEqual({
      text: '2.3s', title: 'Summed over 3,077 keys in 5 batched calls (run d969)',
    })
    expect(lastRunDuration({ ...base, last_run: { ...run, keys: 1, calls: 0, elapsed_seconds: 0.0019 } })).toEqual({
      text: '2ms', title: 'Run d969',
    })
  })

  it('says when a failed run recorded no time, instead of a dash', () => {
    expect(lastRunDuration({ ...base, last_run: { ...run, elapsed_seconds: null, keys: 7, failed: 7, calls: 5 } })).toEqual({
      text: 'not recorded', title: '7 of 7 keys failed before finishing; nothing recorded a time (run d969)',
    })
  })

  it('falls back to the row for an older record, and is empty for a node that never ran', () => {
    expect(lastRunDuration({ ...base, last_materialization: { ...base.last_materialization!, elapsed_seconds: 61 } })).toEqual({ text: '1.0m' })
    expect(lastRunDuration({ ...base, last_materialization: null })).toBeNull()
    expect(lastRunDuration(undefined)).toBeNull()
  })
})
