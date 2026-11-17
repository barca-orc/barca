/**
 * Schedules page logic — pure.
 *
 * A scheduled node is one whose freshness is `schedule` (from `GET /assets`,
 * cron in `schedule`); its next run, last attempt, state and whether it missed a slot
 * (`late_since`) come from `GET /state`, and whether the scheduler has it paused from
 * `GET /schedule`.
 */

import { lastAttemptFailed, shownCacheState, type CacheState } from './cacheStateDisplay'
import { sourceFile } from './pipeline'
import type { ListNode, NodeState, ScheduleJob } from './types'

export interface ScheduleRow {
  id: string
  name: string
  file: string
  kind: ListNode['kind']
  cron: string
  /** The schedule in words, when it is a common shape; null otherwise. */
  human: string | null
  nextRunMs: number | null
  /** It missed a slot: no run has started for the slot at `sinceMs` (`late_since`), and that
   * was longer ago than the grace (`lateGraceMs`). `by`: "late by 12m". While late, the next
   * slot is not when it runs: it waits behind the slots it missed. */
  late: { sinceMs: number; by: string } | null
  /** The latest attempt: how it went, and when (as the server writes it; `Age` draws it). */
  last: { status: string; at: string } | null
  /** `cache.state`, in the CLI's word (`shownCacheState`); null until `/state` has the node. */
  state: CacheState | null
  /** Its last attempt failed: a fact beside the state. */
  lastFailed: boolean
  /** The scheduler skips this node's ticks; `until` (ms) ends a timed pause. */
  paused: { until: number | null } | null
}

const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']

const pad = (n: string) => n.padStart(2, '0')
const isNum = (s: string) => /^\d+$/.test(s)

/** Common cron shapes in words (barca accepts 5 fields, or 6 with leading seconds). */
export function humanizeCron(cron: string): string | null {
  const f = cron.trim().split(/\s+/)
  if (f.length === 6) {
    const [sec, ...rest] = f
    if (rest.every((x) => x === '*')) {
      const step = /^\*\/(\d+)$/.exec(sec ?? '')
      if (step) return `every ${step[1]} seconds`
      if (sec === '*') return 'every second'
    }
    return null
  }
  if (f.length !== 5) return null
  const [min = '', hour = '', dom = '', mon = '', dow = ''] = f
  const minuteStep = /^\*\/(\d+)$/.exec(min)
  if (minuteStep && hour === '*' && dom === '*' && mon === '*' && dow === '*') {
    return `every ${minuteStep[1]} minutes`
  }
  if (min === '*' && hour === '*' && dom === '*' && mon === '*' && dow === '*') return 'every minute'
  if (!isNum(min)) return null
  if (hour === '*' && dom === '*' && mon === '*' && dow === '*') return `hourly at :${pad(min)}`
  if (!isNum(hour) || mon !== '*') return null
  const at = `${pad(hour)}:${pad(min)}`
  if (dom === '*' && dow === '*') return `daily at ${at}`
  if (dom === '*' && isNum(dow) && Number(dow) <= 7) return `weekly on ${DAYS[Number(dow) % 7]} at ${at}`
  if (isNum(dom) && dow === '*') return `monthly on day ${dom} at ${at}`
  return null
}

/** How long a run may take to start after its slot before the schedule reads as late: a
 * minute, or half the interval of a schedule that fires more often than every two minutes. */
export function lateGraceMs(prevRunMs: number | null, nextRunMs: number | null): number {
  const minute = 60_000
  if (prevRunMs === null || nextRunMs === null || nextRunMs <= prevRunMs) return minute
  return Math.max(1_000, Math.min(minute, (nextRunMs - prevRunMs) / 2))
}

/** "late by 12m": how long ago the first missed slot was. */
export function formatLate(sinceMs: number, nowMs: number): string {
  const s = Math.max(0, Math.floor((nowMs - sinceMs) / 1000))
  if (s < 60) return `late by ${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `late by ${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `late by ${h}h ${m % 60}m`
  return `late by ${Math.floor(h / 24)}d`
}

/** Whether a schedule missed its slot, as of `nowMs`: `late_since` (the first slot no run has
 * started for) is further back than the grace, and it is not paused. */
export function lateness(
  state: Pick<NodeState, 'late_since' | 'prev_run' | 'next_run'> | undefined,
  nowMs: number,
  paused: boolean,
): { sinceMs: number; by: string } | null {
  if (!state || paused || state.late_since === null) return null
  const sinceMs = state.late_since * 1000
  const grace = lateGraceMs(
    state.prev_run === null ? null : state.prev_run * 1000,
    state.next_run === null ? null : state.next_run * 1000,
  )
  return nowMs - sinceMs > grace ? { sinceMs, by: formatLate(sinceMs, nowMs) } : null
}

/** Every node with a cron schedule: the late ones first (latest first), then soonest next run. */
export function scheduleRows(
  assets: ListNode[],
  states: NodeState[],
  jobs: ScheduleJob[] = [],
  nowMs: number = Date.now(),
): ScheduleRow[] {
  const byId = new Map(states.map((s) => [s.id, s]))
  const jobById = new Map(jobs.map((j) => [j.id, j]))
  return assets
    .flatMap((a): ScheduleRow[] => {
      if (a.freshness !== 'schedule' || a.schedule === undefined) return []
      const cron = a.schedule
      const st = byId.get(a.id)
      const job = jobById.get(a.id)
      const nextRunSecs = st?.next_run ?? job?.next_fire ?? null
      const nextRunMs = nextRunSecs !== null ? nextRunSecs * 1000 : null
      const last = st?.last_materialization
      const paused = job?.paused ?? false
      const late = lateness(st, nowMs, paused)
      return [
        {
          id: a.id,
          name: st?.name ?? a.id.split(':').pop() ?? a.id,
          file: sourceFile(a.id),
          kind: a.kind,
          cron,
          human: humanizeCron(cron),
          nextRunMs,
          late,
          last: last ? { status: last.status, at: last.created_at } : null,
          state: st ? shownCacheState(st) : null,
          lastFailed: st ? lastAttemptFailed(st) : false,
          paused: paused && job
            ? { until: job.paused_until !== null ? job.paused_until * 1000 : null }
            : null,
        },
      ]
    })
    .sort(
      (a, b) =>
        (a.late?.sinceMs ?? Infinity) - (b.late?.sinceMs ?? Infinity) ||
        (a.nextRunMs ?? Infinity) - (b.nextRunMs ?? Infinity) ||
        a.name.localeCompare(b.name),
    )
}
