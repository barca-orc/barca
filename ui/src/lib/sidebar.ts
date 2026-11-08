/**
 * The sidebar: collapsed or not, which files are open, and the groups listed under a file.
 * The collapse is remembered in this browser; opening a file is not.
 */

import { match } from 'ts-pattern'
import { groupHealth, groupPath, type GroupTree } from './groups'
import { only, type Next } from './program'
import type { Tone } from './tone'
import type { Glyph } from './glyph'
import type { NodeState } from './types'

export const SIDEBAR_KEY = 'barca.sidebar.collapsed'

export interface SidebarModel {
  collapsed: boolean
  /** Files opened by hand; the selected file is always open. */
  expanded: readonly string[]
}

export type SidebarMsg = { type: 'toggle_collapsed' } | { type: 'toggle_file'; file: string }
type SidebarEffect = { type: 'save_collapsed'; collapsed: boolean }

export function updateSidebar(model: SidebarModel, msg: SidebarMsg): Next<SidebarModel, SidebarEffect> {
  return match<SidebarMsg, Next<SidebarModel, SidebarEffect>>(msg)
    .with({ type: 'toggle_collapsed' }, () => {
      const collapsed = !model.collapsed
      return [{ ...model, collapsed }, [{ type: 'save_collapsed', collapsed }]]
    })
    .with({ type: 'toggle_file' }, ({ file }) => only({
      ...model,
      expanded: model.expanded.includes(file) ? model.expanded.filter((f) => f !== file) : [...model.expanded, file],
    }))
    .exhaustive()
}

/** A group as a line of the tree. */
export interface SidebarGroup {
  id: string
  name: string
  depth: number
  /** Null until every member's state is known: no dot beats a dot that says "fine" too soon. */
  tone: Tone | null
  /** How that state is drawn; null with the tone. */
  glyph: Glyph | null
}

/**
 * The groups of `file`, top level first, with the children of the open group and of its
 * ancestors: the tree stays as short as the screen while it follows where you are.
 */
export function fileGroups(
  file: string,
  tree: GroupTree,
  open: string | null,
  leavesOf: ReadonlyMap<string, string[]>,
  byId: Map<string, NodeState>,
): SidebarGroup[] {
  const path = new Set(groupPath(open, tree).map((g) => g.id))
  const out: SidebarGroup[] = []
  const visit = (id: string, depth: number) => {
    const group = tree.groups.get(id)
    if (!group) return
    const leaves = leavesOf.get(id) ?? []
    const known = leaves.every((leaf) => byId.has(leaf))
    const health = known ? groupHealth(leaves, byId) : null
    out.push({ id, name: group.name, depth, tone: health?.tone ?? null, glyph: health?.glyph ?? null })
    if (path.has(id)) for (const member of group.members) visit(member, depth + 1)
  }
  for (const id of tree.roots) if (id.startsWith(`group:${file}:`)) visit(id, 0)
  return out
}
