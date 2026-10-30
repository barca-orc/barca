import { describe, expect, it } from 'vitest'
import { groupTree, leavesByGroup, type NodeGroup } from './groups'
import { replay } from './program'
import { fileGroups, updateSidebar } from './sidebar'
import type { NodeState } from './types'

const group = (id: string, name: string, members: string[]) => ({ id, name, members, output: members[0] }) as NodeGroup
const tree = groupTree([
  group('group:a.py:outer', 'Outer', ['group:a.py:inner', 'a.py:x']),
  group('group:a.py:inner', 'Inner', ['a.py:y']),
  group('group:b.py:other', 'Other', ['b.py:z']),
])
const leaves = leavesByGroup(tree)

describe('sidebar program', () => {
  it('remembers the collapse, not the open files', () => {
    const { model, effects } = replay(updateSidebar, { collapsed: false, expanded: [] }, [
      { type: 'toggle_file', file: 'a.py' },
      { type: 'toggle_collapsed' },
    ])
    expect(model).toEqual({ collapsed: true, expanded: ['a.py'] })
    expect(effects).toEqual([{ type: 'save_collapsed', collapsed: true }])
  })
})

describe('fileGroups', () => {
  it('lists only the file\'s groups, nested ones under the open path', () => {
    expect(fileGroups('a.py', tree, null, leaves, new Map()).map((g) => g.name)).toEqual(['Outer'])
    expect(fileGroups('a.py', tree, 'group:a.py:inner', leaves, new Map()).map((g) => [g.name, g.depth])).toEqual([['Outer', 0], ['Inner', 1]])
  })

  it('shows no status until every member is known', () => {
    const node = { id: 'b.py:z', kind: 'asset', cache: { state: 'cached' }, last_materialization: null, running: null } as unknown as NodeState
    expect(fileGroups('a.py', tree, null, leaves, new Map())[0]!.status).toBeNull()
    expect(fileGroups('b.py', tree, null, leaves, new Map([['b.py:z', node]]))[0]!.status).not.toBeNull()
  })
})
