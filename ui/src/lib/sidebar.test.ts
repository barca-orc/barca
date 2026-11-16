import { describe, expect, it } from 'vitest'
import { groupTree, leavesByGroup, type NodeGroup } from './groups'
import { replay } from './program'
import { SIDEBAR_WIDTH, clampWidth, fileGroups, initialSidebar, listedPipelines, updateSidebar } from './sidebar'
import type { PipelineSummary } from './pipeline'
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
    const { model, effects } = replay(updateSidebar, { collapsed: false, width: 232, expanded: [] }, [
      { type: 'toggle_file', file: 'a.py' },
      { type: 'toggle_collapsed' },
    ])
    expect(model).toEqual({ collapsed: true, width: 232, expanded: ['a.py'] })
    expect(effects).toEqual([{ type: 'save_collapsed', collapsed: true }])
  })

  it('follows a drag and saves the width once, when it ends', () => {
    const { model, effects } = replay(updateSidebar, initialSidebar({ collapsed: null, width: null }), [
      { type: 'resized', width: 260 },
      { type: 'resized', width: 300.4 },
      { type: 'resize_ended' },
    ])
    expect(model.width).toBe(300)
    expect(effects).toEqual([{ type: 'save_width', width: 300 }])
  })

  it('keeps the width in range, by drag or by keyboard, and resets it', () => {
    const start = initialSidebar({ collapsed: '0', width: '250' })
    expect(replay(updateSidebar, start, [{ type: 'resized', width: 40 }]).model.width).toBe(SIDEBAR_WIDTH.min)
    expect(replay(updateSidebar, start, [{ type: 'nudged', by: 16 }]).model.width).toBe(266)
    expect(replay(updateSidebar, start, [{ type: 'width_set', width: 9999 }]).model.width).toBe(SIDEBAR_WIDTH.max)
    const reset = replay(updateSidebar, start, [{ type: 'width_reset' }])
    expect(reset.model.width).toBe(SIDEBAR_WIDTH.initial)
    expect(reset.effects).toEqual([{ type: 'save_width', width: SIDEBAR_WIDTH.initial }])
  })

  it('reads a saved width, and falls back on one that is not a number', () => {
    expect(initialSidebar({ collapsed: '1', width: '320' })).toEqual({ collapsed: true, width: 320, expanded: [] })
    expect(initialSidebar({ collapsed: null, width: 'wide' }).width).toBe(SIDEBAR_WIDTH.initial)
    expect(clampWidth(Number.NaN)).toBe(SIDEBAR_WIDTH.initial)
  })
})

describe('listedPipelines', () => {
  const summary = (file: string, total: number): PipelineSummary => ({ file, name: file, total, shown: null, running: 0 })
  it('leaves out files with none of the kind, except the selected one', () => {
    const all = [summary('a.py', 3), summary('b.py', 0), summary('c.py', 0)]
    expect(listedPipelines(all, null).map((p) => p.file)).toEqual(['a.py'])
    expect(listedPipelines(all, 'c.py').map((p) => p.file)).toEqual(['a.py', 'c.py'])
  })
})

describe('fileGroups', () => {
  it('lists only the file\'s groups, nested ones under the open path', () => {
    expect(fileGroups('a.py', tree, null, leaves, new Map()).map((g) => g.name)).toEqual(['Outer'])
    expect(fileGroups('a.py', tree, 'group:a.py:inner', leaves, new Map()).map((g) => [g.name, g.depth])).toEqual([['Outer', 0], ['Inner', 1]])
  })

  it('shows no status until every member is known', () => {
    const node = { id: 'b.py:z', kind: 'asset', cache: { state: 'cached' }, last_materialization: null, running: null } as unknown as NodeState
    expect(fileGroups('a.py', tree, null, leaves, new Map())[0]!.tone).toBeNull()
    expect(fileGroups('b.py', tree, null, leaves, new Map([['b.py:z', node]]))[0]!.tone).toBe('success')
  })
})
