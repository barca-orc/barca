import { formatAbsolute, formatAgo } from '@/lib/time'

export interface AgeProps {
  /** A time as the server writes it. */
  at: string
  nowMs: number
}

/** How long ago, with the absolute time on hover: the one way a time is shown. */
export function Age({ at, nowMs }: AgeProps) {
  return <time dateTime={at} title={formatAbsolute(at)}>{formatAgo(at, nowMs)}</time>
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
