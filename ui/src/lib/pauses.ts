/**
 * Paused assets (`GET /pauses`, `barca pause`) and an asset's blast radius: what is downstream
 * of it, and which schedules would compute it or what reads it. Pure.
 */

import { consumersOf, downstreamIds } from './health'
import type { GraphLink } from './health'
import type { ScheduleJob } from './types'

/** One paused node of `GET /pauses`. */
export interface AssetPause {
  id: string
  /** Unix seconds. */
  paused_at: number
  /** What the pause named: the node, the node a downstream pause started from, or a file. */
  via: string
}

/** `GET /pauses`, and the answer of `POST /pauses` (`added`) and `POST /pauses/resume` (`removed`). */
export interface Pauses {
  total: number
  paused: AssetPause[]
  added?: string[]
  removed?: string[]
}

/** The body of `POST /pauses` and `POST /pauses/resume`. */
export interface PauseRequest {
  target?: string
  downstream?: boolean
  all?: boolean
}

/** The source file of a node id (`dir/pipeline.py:name` → `dir/pipeline.py`). */
export function fileOf(id: string): string {
  const at = id.lastIndexOf(':')
  return at < 0 ? id : id.slice(0, at)
}

/** How a schedule touches the asset: it is the asset, it reads it, or the asset reads it. */
export type ScheduleRelation = 'self' | 'downstream' | 'upstream'

export interface AffectedSchedule {
  job: ScheduleJob
  relation: ScheduleRelation
}

export interface BlastRadius {
  /** Every node downstream of the asset, transitively, in graph order. */
  downstream: string[]
  /** The schedules whose ticks compute the asset or something downstream of it, soonest first. */
  schedules: AffectedSchedule[]
}

/** Every node upstream of `id`, transitively (not `id` itself). */
function upstreamIds(id: string, inputs: ReadonlyMap<string, readonly string[]>): Set<string> {
  const seen = new Set<string>()
  const stack = [...(inputs.get(id) ?? [])]
  while (stack.length) {
    const next = stack.pop()!
    if (seen.has(next)) continue
    seen.add(next)
    for (const i of inputs.get(next) ?? []) if (!seen.has(i)) stack.push(i)
  }
  return seen
}

/**
 * What a failure of `id` reaches: its downstream nodes, and the schedules that would compute it
 * or them. A schedule upstream of it counts too: its tick reconciles the `Always` nodes below it.
 */
export function blastRadius(id: string, graph: readonly GraphLink[], jobs: readonly ScheduleJob[]): BlastRadius {
  const below = downstreamIds(id, consumersOf(graph))
  const above = upstreamIds(id, new Map(graph.map((n) => [n.id, n.inputs])))
  const downstream = graph.filter((n) => below.has(n.id)).map((n) => n.id)
  const schedules: AffectedSchedule[] = []
  for (const job of jobs) {
    const relation: ScheduleRelation | null =
      job.id === id ? 'self' : below.has(job.id) ? 'downstream' : above.has(job.id) ? 'upstream' : null
    if (relation) schedules.push({ job, relation })
  }
  schedules.sort((a, b) => (a.job.next_fire ?? Infinity) - (b.job.next_fire ?? Infinity))
  return { downstream, schedules }
}

/** The ids of `GET /pauses`, for a lookup. */
export function pausedIds(pauses: Pauses | undefined): ReadonlySet<string> {
  return new Set(pauses?.paused.map((p) => p.id) ?? [])
}

/** The banner's words: `1 asset paused. Scheduled runs skip it until it is resumed.` (plural
 * for N); null when nothing is paused. */
export function pausedBanner(total: number): { headline: string; detail: string } | null {
  if (total <= 0) return null
  return total === 1
    ? { headline: '1 asset paused.', detail: 'Scheduled runs skip it until it is resumed.' }
    : { headline: `${total} assets paused.`, detail: 'Scheduled runs skip them until they are resumed.' }
}
