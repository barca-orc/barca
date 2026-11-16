import { useTimeZone } from '@/context/timeZone'
import { useNow } from '@/hooks/useNow'
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
  at: string | null
  failingSince: string | null
  failures: number
  nowMs: number
}

/** The age a table row answers: its last success and, while failing, since when and how often. */
export function LastSuccess({ at, failingSince, failures, nowMs }: LastSuccessProps) {
  return <>
    {at ? <Age at={at} nowMs={nowMs} /> : <span className="barca-cell-sub">never</span>}
    {failingSince && (
      <div className="barca-cell-sub">
        failing since <Age at={failingSince} nowMs={nowMs} /> · {failures} {failures === 1 ? 'run' : 'runs'}
      </div>
    )}
  </>
}
