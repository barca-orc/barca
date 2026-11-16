/**
 * Schedules page logic — pure.
 *
 * A scheduled node is one whose freshness is `schedule` (from `GET /assets`,
 * cron in `schedule`); its next run, last attempt and state come from
 * `GET /state`, and whether the scheduler has it paused from `GET /schedule`.
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

/** Every node with a cron schedule, soonest next run first. */
export function scheduleRows(
  assets: ListNode[],
  states: NodeState[],
  jobs: ScheduleJob[] = [],
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
      return [
        {
          id: a.id,
          name: st?.name ?? a.id.split(':').pop() ?? a.id,
          file: sourceFile(a.id),
          kind: a.kind,
          cron,
          human: humanizeCron(cron),
          nextRunMs,
          last: last ? { status: last.status, at: last.created_at } : null,
          state: st ? shownCacheState(st) : null,
          lastFailed: st ? lastAttemptFailed(st) : false,
          paused: job?.paused
            ? { until: job.paused_until !== null ? job.paused_until * 1000 : null }
            : null,
        },
      ]
    })
    .sort(
      (a, b) =>
        (a.nextRunMs ?? Infinity) - (b.nextRunMs ?? Infinity) || a.name.localeCompare(b.name),
    )
}
