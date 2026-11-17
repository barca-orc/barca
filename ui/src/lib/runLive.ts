/**
 * What a run in progress is working on now, for its panel: the nodes of its `active_steps`
 * (`GET /runs?status=running`) as rows, and their groups. The run's explanation itself (why its
 * steps ran, its retries, its progress and ETA, its slowest nodes) is computed by the server
 * (`RunView.explanation`, `barca-core`'s `run_explanation`); this file only lays out rows.
 */

import { plural } from './plural'
import type { ActiveStep, NodeGroup } from './types'

export interface ActiveNode {
  nodeId: string
  /** `3,120 of 5,000 keys`, null for an unpartitioned node. */
  keys: string | null
  /** 0–1 of its keys done, null for an unpartitioned node. */
  share: number | null
}

export interface GroupActivity {
  group: string
  running: number
  queued: number
}

export interface RunActivityNow {
  running: ActiveNode[]
  queued: number
  /** Keys with a result of the partitioned nodes in flight. */
  keys: { done: number; total: number } | null
  /** The groups of the nodes in flight, busiest first; empty without groups. */
  groups: GroupActivity[]
}

/** How many nodes the overview lists as executing now. */
export const ACTIVE_SHOWN = 8

/** The innermost group of each node: the group with the fewest members that has it. */
export function groupOfNodes(groups: readonly NodeGroup[]): Map<string, string> {
  const out = new Map<string, { name: string; size: number }>()
  for (const g of groups) {
    for (const m of g.members) {
      const known = out.get(m)
      if (!known || g.members.length < known.size) out.set(m, { name: g.name, size: g.members.length })
    }
  }
  return new Map([...out].map(([node, g]) => [node, g.name]))
}

/** The rows of a live run's "Executing now" and "By group", from its `active_steps`. */
export function activityNow(active: readonly ActiveStep[], groups: readonly NodeGroup[]): RunActivityNow {
  let keysDone = 0
  let keysTotal = 0
  for (const a of active) {
    if (a.partitions) {
      keysDone += a.partitions.done
      keysTotal += a.partitions.total
    }
  }
  const running: ActiveNode[] = active
    .filter((a) => a.step === 'running')
    .map((a) => ({
      nodeId: a.node_id,
      keys: a.partitions ? `${a.partitions.done.toLocaleString()} of ${plural(a.partitions.total, 'key')}` : null,
      share: a.partitions && a.partitions.total > 0 ? a.partitions.done / a.partitions.total : null,
    }))
    // Furthest from done first: what holds the run up.
    .sort((x, y) => (x.share ?? 1) - (y.share ?? 1) || x.nodeId.localeCompare(y.nodeId))
  const byGroup = groupOfNodes(groups)
  const activity = new Map<string, GroupActivity>()
  for (const a of active) {
    const group = byGroup.get(a.node_id)
    if (!group) continue
    const g = activity.get(group) ?? { group, running: 0, queued: 0 }
    if (a.step === 'running') g.running += 1
    else g.queued += 1
    activity.set(group, g)
  }
  return {
    running,
    queued: active.filter((a) => a.step === 'queued').length,
    keys: keysTotal > 0 ? { done: keysDone, total: keysTotal } : null,
    groups: [...activity.values()].sort((a, b) => b.running + b.queued - (a.running + a.queued) || a.group.localeCompare(b.group)),
  }
}
