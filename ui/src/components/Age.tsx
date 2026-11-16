import { useTimeZone } from '@/context/timeZone'
import { successOfLabel, type AgeCell } from '@/lib/health'
import { formatAbsolute, formatAgo, formatInstant } from '@/lib/time'

export interface AgeProps {
  /** A time as the server writes it. */
  at: string
  nowMs: number
}

/** How long ago, with the absolute time on hover: the one way a time is shown. */
export function Age({ at, nowMs }: AgeProps) {
  const { zone } = useTimeZone()
  return <time dateTime={at} title={formatAbsolute(at, zone)}>{formatAgo(at, nowMs)}</time>
}

export interface AbsoluteProps {
  /** A time as the server writes it, or ms since the epoch; `null` is shown as a dash. */
  at: string | number | null
}

/** An absolute time in the reader's zone, zone named: run start and finish, next run, expiry. */
export function Absolute({ at }: AbsoluteProps) {
  const { zone } = useTimeZone()
  if (at === null) return <>–</>
  if (typeof at === 'number') return <time dateTime={new Date(at).toISOString()}>{formatInstant(at, zone)}</time>
  return <time dateTime={at}>{formatAbsolute(at, zone)}</time>
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
