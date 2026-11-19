/**
 * Paused assets (`GET /pauses`, `barca pause`) and what a pause of an asset would cover, as the
 * server says it (`covers`). Pure.
 */

import type { PauseCover } from './generated/PauseCover'
import type { PauseRelation } from './generated/PauseRelation'
import type { Pauses, ScheduleJob } from './types'

export type { PauseCover, PauseRelation }

/** The source file of a node id (`dir/pipeline.py:name` → `dir/pipeline.py`). */
export function fileOf(id: string): string {
  const at = id.lastIndexOf(':')
  return at < 0 ? id : id.slice(0, at)
}

/** A schedule a pause would hold work back from, with what `GET /schedule` says of it now. */
export interface AffectedSchedule {
  job: ScheduleJob
  relation: PauseRelation
}

/**
 * The schedules a pause covers (`covers.schedules` of `GET /pauses?target=`, found by the
 * scheduler's own walk in barca-core) joined with their jobs (`GET /schedule`), soonest next
 * tick first. Which schedules, and how each touches the node, is the server's; this only looks
 * each one up and orders the list. A schedule `GET /schedule` does not list yet is left out.
 */
export function affectedSchedules(cover: PauseCover | undefined, jobs: readonly ScheduleJob[]): AffectedSchedule[] {
  const byId = new Map(jobs.map((job) => [job.id, job]))
  return (cover?.schedules ?? [])
    .flatMap(({ id, relation }) => {
      const job = byId.get(id)
      return job ? [{ job, relation }] : []
    })
    .sort((a, b) => (a.job.next_fire ?? Infinity) - (b.job.next_fire ?? Infinity))
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
