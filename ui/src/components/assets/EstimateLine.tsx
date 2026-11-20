import { Age, KeyValue } from '@/components'
import { useTimeZone } from '@/context/timeZone'
import { noEstimateWord } from '@/lib/causeDisplay'
import { formatEstimate, formatSlot } from '@/lib/time'
import type { NoEstimate } from '@/lib/generated/NoEstimate'

export interface EstimateLineProps {
  /** The node's `stale_since`: since when it has been out of date; `null` when fresh or unknown. */
  staleSince: string | null
  /** The node's `expected_fresh_at`: the server's estimate; `null` when it promises none. */
  expectedFreshAt: string | null
  /** The node's `recheck_at`: when a tick next looks at it; `null` when none will. */
  recheckAt: string | null
  /** The node's `no_estimate`: why it has no `expected_fresh_at`; `null` when fresh or estimated. */
  noEstimate?: NoEstimate | null
  nowMs: number
}

/**
 * When the node went out of date and when it will be fresh again, as the server estimated
 * them: each time is one field of its answer, shown as it is. "Fresh" is said only of
 * `expected_fresh_at`, always as an estimate (`~15:05`, to the minute and rounded up, never a
 * second it does not know); where the server gives only `recheck_at`, a tick will look at the
 * node again and the word is "rechecked", at the slot's exact time. A node with no estimate
 * says why (`no_estimate`), never a blank. Which of these a node gets, and every time and
 * reason, is decided in `barca-service`, never here.
 */
export function EstimateLine({ staleSince, expectedFreshAt, recheckAt, noEstimate = null, nowMs }: EstimateLineProps) {
  const { zone } = useTimeZone()
  if (staleSince === null && expectedFreshAt === null && recheckAt === null && noEstimate === null) return null
  return (
    <>
      {staleSince !== null && (
        <KeyValue label="out of date">
          <span data-stale-since={staleSince}>since <Age at={staleSince} nowMs={nowMs} /></span>
        </KeyValue>
      )}
      {expectedFreshAt !== null && (
        <KeyValue label="fresh">
          <span data-expected-fresh-at={expectedFreshAt}>{formatEstimate(expectedFreshAt, zone, nowMs)}</span>{' '}
          <span className="barca-cell-sub">· <Age at={expectedFreshAt} nowMs={nowMs} />, an estimate</span>
        </KeyValue>
      )}
      {expectedFreshAt === null && noEstimate !== null && (
        <KeyValue label="no estimate">
          <span data-no-estimate={noEstimate}>{noEstimateWord(noEstimate)}</span>
        </KeyValue>
      )}
      {expectedFreshAt === null && recheckAt !== null && (
        <KeyValue label="rechecked">
          <span data-recheck-at={recheckAt}>{formatSlot(recheckAt, zone, nowMs)}</span>{' '}
          <span className="barca-cell-sub">· <Age at={recheckAt} nowMs={nowMs} />, by the next tick that reaches it</span>
        </KeyValue>
      )}
    </>
  )
}
