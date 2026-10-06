/**
 * Schedules page logic — pure.
 *
 * A scheduled node is one whose freshness is `Schedule(cron)` (from
 * `GET /assets`); its next run, last attempt and state come from `GET /state`.
 */

import { formatAgo, severityOf, type Severity } from './assetTable'
import { sourceFile } from './pipeline'
import type { AssetSummary, NodeState } from './types'

export interface ScheduleRow {
  id: string
  name: string
  file: string
  kind: AssetSummary['kind']
  cron: string
  /** The schedule in words, when it is a common shape; null otherwise. */
  human: string | null
  nextRunMs: number | null
  /** "in 5m", relative to the data's fetch time. */
  nextIn: string | null
  last: { status: string; ago: string } | null
  severity: Severity | null
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

export function formatCountdown(atMs: number, nowMs: number): string {
  const s = Math.floor((atMs - nowMs) / 1000)
  if (s < 0) return 'due now'
  if (s < 60) return `in ${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `in ${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `in ${h}h ${m % 60}m`
  return `in ${Math.floor(h / 24)}d`
}

/** Every node with a cron schedule, soonest next run first. */
export function scheduleRows(
  assets: AssetSummary[],
  states: NodeState[],
  nowMs: number,
): ScheduleRow[] {
  const byId = new Map(states.map((s) => [s.id, s]))
  return assets
    .flatMap((a): ScheduleRow[] => {
      if (a.freshness.type !== 'Schedule') return []
      const st = byId.get(a.id)
      const nextRunMs = st?.next_run != null ? st.next_run * 1000 : null
      const last = st?.last_materialization
      return [
        {
          id: a.id,
          name: st?.name ?? a.id.split(':').pop() ?? a.id,
          file: sourceFile(a.id),
          kind: a.kind,
          cron: a.freshness.value,
          human: humanizeCron(a.freshness.value),
          nextRunMs,
          nextIn: nextRunMs === null ? null : formatCountdown(nextRunMs, nowMs),
          last: last ? { status: last.status, ago: formatAgo(last.created_at, nowMs) } : null,
          severity: st ? severityOf(st) : null,
        },
      ]
    })
    .sort(
      (a, b) =>
        (a.nextRunMs ?? Infinity) - (b.nextRunMs ?? Infinity) || a.name.localeCompare(b.name),
    )
}
