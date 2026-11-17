import { Age, KeyValue } from '@/components'

export interface EstimateLineProps {
  /** The node's `stale_since`: since when it has been out of date; `null` when fresh or unknown. */
  staleSince: string | null
  /** The node's `expected_fresh_at`: the server's estimate; `null` when it promises none. */
  expectedFreshAt: string | null
  /** The node's `recheck_at`: when a tick next looks at it; `null` when none will. */
  recheckAt: string | null
  nowMs: number
}

/**
 * When the node went out of date and when it will be fresh again, as the server estimated
 * them: each time is one field of its answer, shown as it is. "Fresh" is said only of
 * `expected_fresh_at`, always as an estimate ("about"); where the server gives only
 * `recheck_at`, a tick will look at the node again and the word is "rechecked". Which of the
 * two a node gets, and every time, is decided in `barca-core`, never here.
 */
export function EstimateLine({ staleSince, expectedFreshAt, recheckAt, nowMs }: EstimateLineProps) {
  if (staleSince === null && expectedFreshAt === null && recheckAt === null) return null
  return (
    <>
      {staleSince !== null && (
        <KeyValue label="out of date">
          <span data-stale-since={staleSince}>since <Age at={staleSince} nowMs={nowMs} /></span>
        </KeyValue>
      )}
      {expectedFreshAt !== null ? (
        <KeyValue label="expected fresh">
          <span data-expected-fresh-at={expectedFreshAt}>about <Age at={expectedFreshAt} nowMs={nowMs} /></span>{' '}
          <span className="barca-cell-sub">an estimate</span>
        </KeyValue>
      ) : recheckAt !== null && (
        <KeyValue label="rechecked">
          <span data-recheck-at={recheckAt}><Age at={recheckAt} nowMs={nowMs} /></span>{' '}
          <span className="barca-cell-sub">by the next tick that reaches it</span>
        </KeyValue>
      )}
    </>
  )
}
