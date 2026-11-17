/**
 * Schedules page logic — pure.
 *
 * A scheduled node is one whose freshness is `schedule` (from `GET /assets`,
 * cron in `schedule`). Where its schedule stands (its last tick and that tick's run, whether
 * it is late and since when) is decided by the server, once, and read from the `schedule`
 * block of `GET /state`; this module words it and orders the rows. Whether the scheduler has
 * it paused is read from `GET /schedule`, which the pause control updates.
 */

import { lastAttemptFailed, shownCacheState, type CacheState } from './cacheStateDisplay'
import { sourceFile } from './pipeline'
import { formatAbout, parseTime } from './time'
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
  /** The server says it is late (`schedule.late`): no tick fired for the slot at `sinceMs`
   * (`schedule.late_since`). `by`: "late by 12m". While late, the next slot is not when it
   * runs: it waits behind the slots it missed. */
  late: { sinceMs: number; by: string } | null
  /** Its newest tick (`schedule.last_fired`, as the server writes it; `Age` draws it), and the
   * run that tick started: its status (`pending` while it waits in the run queue) and id. */
  last: { status: string | null; at: string; runId: string | null } | null
  /** `cache.state`, in the CLI's word (`shownCacheState`); null until `/state` has the node. */
  state: CacheState | null
  /** Its last attempt failed: a fact beside the state. */
  lastFailed: boolean
  /** The scheduler skips this node's ticks; `until` (ms) ends a timed pause. */
  paused: { until: number | null } | null
  /**
   * How long its ticks' runs typically wait for a run slot (`schedule.typical_queue_wait_seconds`),
   * as an estimate: `~12s`; null when they do not wait or nothing is recorded.
   */
  queueWait: string | null
  /** How long its ticks' runs typically take (`schedule.typical_run_seconds`): `~17s`; null with none recorded. */
  typicalRun: string | null
  /** The server says its ticks wait longer than they are apart (`schedule.queue_bound`). */
  queueBound: boolean
}

/** What to do about a queue-bound schedule: the hint `barca status --rollup` prints for `raise max_runs`. */
export const QUEUE_BOUND_HINT = 'Its ticks wait for a run slot longer than they are apart. Give the server more run slots (barca serve --max-runs N, or [server] max_runs in barca.toml), or space the schedules.'

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

/** The status of a tick's run in the page's words: a run that waits for a run slot is `queued`. */
export function lastStatusLabel(status: string): string {
  return status === 'pending' ? 'queued' : status
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

/**
 * Every node with a cron schedule: the ones the server calls late first (longest late first),
 * then the queue-bound ones, then soonest next slot. Late and queue-bound are the server's
 * words (`schedule.late`, `schedule.queue_bound`); this only puts them on top.
 */
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
      // Where the schedule stands is the server's answer (`schedule` of `GET /state`, the
      // same values as `GET /schedule` and `barca status`): nothing here compares times.
      const stands = st?.schedule
      const nextRunSecs = st?.next_run ?? job?.next_fire ?? null
      const nextRunMs = nextRunSecs !== null ? nextRunSecs * 1000 : null
      const lateSinceMs = stands?.late && stands.late_since ? parseTime(stands.late_since) : null
      const paused = job?.paused ?? stands?.paused ?? false
      const pausedUntilMs = job
        ? job.paused_until !== null ? job.paused_until * 1000 : null
        : stands?.paused_until ? parseTime(stands.paused_until) : null
      return [
        {
          id: a.id,
          name: st?.name ?? a.id.split(':').pop() ?? a.id,
          file: sourceFile(a.id),
          kind: a.kind,
          cron,
          human: humanizeCron(cron),
          nextRunMs,
          late: lateSinceMs !== null ? { sinceMs: lateSinceMs, by: formatLate(lateSinceMs, nowMs) } : null,
          last: stands?.last_fired
            ? { status: stands.last_status, at: stands.last_fired, runId: stands.last_run_id }
            : null,
          state: st ? shownCacheState(st) : null,
          lastFailed: st ? lastAttemptFailed(st) : false,
          paused: paused ? { until: pausedUntilMs } : null,
          queueWait: stands && stands.typical_queue_wait_seconds > 0 ? formatAbout(stands.typical_queue_wait_seconds) : null,
          typicalRun: stands && stands.typical_run_seconds !== null ? formatAbout(stands.typical_run_seconds) : null,
          queueBound: stands?.queue_bound ?? false,
        },
      ]
    })
    .sort(
      (a, b) =>
        (a.late?.sinceMs ?? Infinity) - (b.late?.sinceMs ?? Infinity) ||
        Number(b.queueBound) - Number(a.queueBound) ||
        (a.nextRunMs ?? Infinity) - (b.nextRunMs ?? Infinity) ||
        a.name.localeCompare(b.name),
    )
}
