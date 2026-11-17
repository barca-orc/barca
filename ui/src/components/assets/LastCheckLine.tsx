import { Age, KeyValue } from '@/components'
import { checkOutcomeLabel } from '@/lib/healthDisplay'
import { shortName } from '@/lib/graph'
import type { LastCheck } from '@/lib/generated/LastCheck'

export interface LastCheckLineProps {
  /** The node's `last_check`, as the server sends it. */
  check: LastCheck
  nowMs: number
}

/**
 * What the newest tick that checked the node found: `checked 3m ago, up to date`. The record
 * is shown as it is; whether it decides the node's health is the server's answer (`health`,
 * `health_reason: "checked_up_to_date"`), never worked out here.
 */
export function LastCheckLine({ check, nowMs }: LastCheckLineProps) {
  return (
    <KeyValue label="last tick">
      <span data-last-check={check.outcome}>
        checked <Age at={check.at} nowMs={nowMs} />, {checkOutcomeLabel(check.outcome)}
      </span>{' '}
      <span className="barca-cell-sub">by the schedule of {shortName(check.schedule)}</span>
    </KeyValue>
  )
}
