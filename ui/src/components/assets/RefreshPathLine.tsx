import { Age, KeyValue } from '@/components'
import { shortName } from '@/lib/graph'
import type { RefreshPath } from '@/lib/generated/RefreshPath'

export interface RefreshPathLineProps {
  /** The node's `refresh_path`, as the server sends it; `null` when no tick will refresh it. */
  path: RefreshPath | null
  nowMs: number
}

/**
 * What will bring the node up to date: the schedule whose tick targets it, when that next
 * fires, and what the server says of it (`paused`, not `live`, ticks that missed the node).
 * Each part is one field of the server's answer, shown as it is; which schedule reaches the
 * node and what counts as a missed tick are decided in `barca-service`, never here.
 */
export function RefreshPathLine({ path, nowMs }: RefreshPathLineProps) {
  if (!path) {
    return (
      <KeyValue label="refresh path">
        <span data-refresh-path="none">no refresh path</span>{' '}
        <span className="barca-cell-sub">no scheduled tick brings it up to date</span>
      </KeyValue>
    )
  }
  return (
    <KeyValue label="refresh path">
      <span data-refresh-path={path.schedule}>the schedule of {shortName(path.schedule)}</span>
      {path.next_tick !== null && <span className="barca-cell-sub"> · next tick <Age at={path.next_tick} nowMs={nowMs} /></span>}
      {path.paused && <span className="barca-cell-sub"> · paused</span>}
      {(path.state === "held" || path.state === "overrunning") && <span className="barca-cell-sub"> · {path.state}</span>}
      {!path.live && path.state !== "held" && <span className="barca-cell-sub"> · not firing</span>}
      {path.missed_ticks > 0 && <span className="barca-cell-sub"> · missed {path.missed_ticks} {path.missed_ticks === 1 ? 'tick' : 'ticks'}</span>}
    </KeyValue>
  )
}
