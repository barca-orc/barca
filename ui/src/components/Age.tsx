import { useTimeZone } from '@/context/timeZone'
import { useNow } from '@/hooks/useNow'
import { successOfLabel, type AgeCell } from '@/lib/health'
import { formatInstant, formatRelative, parseTime } from '@/lib/time'

export interface AgeProps {
  /** A time as the server writes it, or ms since the epoch; `null` is shown as a dash. */
  at: string | number | null
  /**
   * What the time is relative to: the fetch time of the data it came with, so it changes with
   * the data. Without it, the time now (refreshed every 15 s).
   */
  nowMs?: number
}

/**
 * The one way a time is shown: relative (`3h ago`, `in 5m`), the absolute time in the reader's
 * zone on hover. A run's start, a next run, a token's expiry all read the same.
 */
export function Age({ at, nowMs }: AgeProps) {
  const { zone } = useTimeZone()
  const ticking = useNow(nowMs === undefined)
  if (at === null) return <>–</>
  const ms = typeof at === 'number' ? at : parseTime(at)
  if (ms === null) return <>{at}</>
  return <time dateTime={new Date(ms).toISOString()} title={formatInstant(ms, zone)}>{formatRelative(ms, nowMs ?? ticking)}</time>
}

export interface LastSuccessProps {
  /** What the cell says (`ageCell` in `lib/health.ts`). */
  cell: AgeCell
  nowMs: number
}

/**
 * The age a table row answers: its last success, whose it is when not the row's own (`any key`,
 * `oldest member`), and, while failing, since when and how many runs in a row.
 */
export function LastSuccess({ cell, nowMs }: LastSuccessProps) {
  const of = cell.success ? successOfLabel(cell.success.of) : null
  return <>
    {cell.success ? <><Age at={cell.success.at} nowMs={nowMs} />{of && <span className="barca-cell-sub"> · {of}</span>}</> : <span className="barca-cell-sub">never</span>}
    {cell.failing && (
      <div className="barca-cell-sub barca-age-failing">
        {cell.failing.lead} since <Age at={cell.failing.since} nowMs={nowMs} />{cell.failing.tail}
      </div>
    )}
  </>
}
