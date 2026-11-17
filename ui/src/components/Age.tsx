import { useTimeZone } from '@/context/timeZone'
import { useNow } from '@/hooks/useNow'
import { NO_TIME, formatInstant, formatRelative, isInstant, parseTime } from '@/lib/time'

export interface AgeProps {
  /** A time as the server writes it, or ms since the epoch; `null` (or a zero time) is shown as a dash. */
  at: string | number | null
  /**
   * What the time is relative to: the fetch time of the data it came with, so it changes with
   * the data. Without it, or while that data is still loading (`0`, a query's `dataUpdatedAt`
   * before its first answer), the time now (refreshed every 15 s).
   */
  nowMs?: number
}

/**
 * The one way a time is shown: relative (`3h ago`, `in 5m`), the absolute time in the reader's
 * zone on hover. A run's start, a next run, a token's expiry all read the same.
 */
export function Age({ at, nowMs }: AgeProps) {
  const { zone } = useTimeZone()
  const given = isInstant(nowMs)
  const ticking = useNow(!given)
  if (at === null) return <>{NO_TIME}</>
  const ms = typeof at === 'number' ? at : parseTime(at)
  if (ms === null) return <>{at}</>
  if (!isInstant(ms)) return <>{NO_TIME}</>
  return <time dateTime={new Date(ms).toISOString()} title={formatInstant(ms, zone)}>{formatRelative(ms, given ? nowMs : ticking)}</time>
}
