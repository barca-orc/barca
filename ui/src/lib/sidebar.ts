/**
 * The sidebar: collapsed or not, how wide, which files are open, and the groups listed under a
 * file. The collapse and the width are remembered in this browser; opening a file is not.
 */

import type { MapPart } from './causeDisplay'
import { match } from 'ts-pattern'
import { groupHealth, groupPath, type GroupScope, type GroupTree } from './groups'
import { only, type Next } from './program'
import type { PipelineSummary } from './pipeline'
import type { Tone } from './tone'
import type { Glyph } from './glyph'

export const SIDEBAR_KEY = 'barca.sidebar.collapsed'
export const SIDEBAR_WIDTH_KEY = 'barca.sidebar.width'

/** The sidebar's width in px: the default, and the range a drag or the keyboard keeps it in. */
export const SIDEBAR_WIDTH = { initial: 232, min: 180, max: 480 } as const
/** How far one arrow key moves the sidebar's edge; with Shift, four times as far. */
export const SIDEBAR_STEP = 16

export interface SidebarModel {
  collapsed: boolean
  /** The open sidebar's width in px, within `SIDEBAR_WIDTH`. */
  width: number
  /** Files opened by hand; the selected file is always open. */
  expanded: readonly string[]
}

export type SidebarMsg =
  | { type: 'toggle_collapsed' }
  | { type: 'toggle_file'; file: string }
  /** The edge is being dragged: the width follows, and is saved when the drag ends. */
  | { type: 'resized'; width: number }
  | { type: 'resize_ended' }
  /** An arrow key on the edge, in px (negative is narrower). */
  | { type: 'nudged'; by: number }
  /** Home and End on the edge: the narrowest or the widest. */
  | { type: 'width_set'; width: number }
  /** A double click on the edge. */
  | { type: 'width_reset' }

export type SidebarEffect = { type: 'save_collapsed'; collapsed: boolean } | { type: 'save_width'; width: number }

/** `width`, whole and within the range; anything that is not a number is the default. */
export function clampWidth(width: number): number {
  if (!Number.isFinite(width)) return SIDEBAR_WIDTH.initial
  return Math.round(Math.min(SIDEBAR_WIDTH.max, Math.max(SIDEBAR_WIDTH.min, width)))
}

/** The model a page opens with, from what this browser saved (null when nothing was, or it cannot be read). */
export function initialSidebar(saved: { collapsed: string | null; width: string | null }): SidebarModel {
  return {
    collapsed: saved.collapsed === '1',
    width: saved.width === null ? SIDEBAR_WIDTH.initial : clampWidth(Number(saved.width)),
    expanded: [],
  }
}

export function updateSidebar(model: SidebarModel, msg: SidebarMsg): Next<SidebarModel, SidebarEffect> {
  const sized = (width: number): Next<SidebarModel, SidebarEffect> => {
    const next = clampWidth(width)
    return [{ ...model, width: next }, [{ type: 'save_width', width: next }]]
  }
  return match<SidebarMsg, Next<SidebarModel, SidebarEffect>>(msg)
    .with({ type: 'toggle_collapsed' }, () => {
      const collapsed = !model.collapsed
      return [{ ...model, collapsed }, [{ type: 'save_collapsed', collapsed }]]
    })
    .with({ type: 'toggle_file' }, ({ file }) => only({
      ...model,
      expanded: model.expanded.includes(file) ? model.expanded.filter((f) => f !== file) : [...model.expanded, file],
    }))
    .with({ type: 'resized' }, ({ width }) => only({ ...model, width: clampWidth(width) }))
    .with({ type: 'resize_ended' }, () => sized(model.width))
    .with({ type: 'nudged' }, ({ by }) => sized(model.width + by))
    .with({ type: 'width_set' }, ({ width }) => sized(width))
    .with({ type: 'width_reset' }, () => sized(SIDEBAR_WIDTH.initial))
    .exhaustive()
}

/**
 * The files the sidebar lists on a page: those with something of the page's kind, and the
 * selected one whatever it has (so the selection is always on screen). A file of none, listed
 * at 0, is only noise in a long list.
 */
export function listedPipelines(pipelines: readonly PipelineSummary[], selected: string | null): PipelineSummary[] {
  return pipelines.filter((p) => p.total > 0 || p.file === selected)
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
  /** The cause (or fresh) its line is marked with, in the health map's colour; null while a run leads, or with the tone. */
  part: MapPart | null
}

/**
 * The groups of `file`, top level first, with the children of the open group and of its
 * ancestors: the tree stays as short as the screen while it follows where you are.
 */
export function fileGroups(
  file: string,
  tree: GroupTree,
  open: string | null,
  entries: ReadonlyMap<string, GroupScope>,
): SidebarGroup[] {
  const path = new Set(groupPath(open, tree).map((g) => g.id))
  const out: SidebarGroup[] = []
  const visit = (id: string, depth: number) => {
    const group = tree.groups.get(id)
    if (!group) return
    // The group's entry of the project's rollup: none until it has loaded.
    const entry = entries.get(id)
    const health = entry ? groupHealth(entry) : null
    out.push({ id, name: group.name, depth, tone: health?.tone ?? null, glyph: health?.glyph ?? null, part: health?.kind === 'group' ? health.part ?? null : null })
    if (path.has(id)) for (const member of group.members) visit(member, depth + 1)
  }
  for (const id of tree.roots) if (id.startsWith(`group:${file}:`)) visit(id, 0)
  return out
}
