import { Link } from 'react-router'

/** Keep an incomplete live transcript distinct from the canonical run record. */
export function StreamNotice({ notice, runId }: { notice?: string; runId?: string }) {
  if (!notice) return null
  return <p role="status" className="barca-note barca-stream-notice">
    {notice}{' '}{runId && <Link to={`/runs?run=${encodeURIComponent(runId)}&event_gap=1`}>View recorded logs</Link>}
  </p>
}
