import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { useQueue } from '@/hooks/useQueue'
import { QUEUE_POLL_IDLE_MS, queueStatus, queueStatusLabel, waitLabel, type QueueStatus as Status } from '@/lib/queue'
import { StatusGlyph } from './StatusGlyph'

/** How often the oldest wait is re-counted between polls, ms. */
const TICK_MS = 15_000

/**
 * barca · QueueStatus
 * The run queue in brief on every page while runs wait (or the queue is paused or draining):
 * `Queue · 18 waiting`, then `1 running · oldest 20m`; links to the Runs page, which lists them.
 * Asks `GET /queue` every 10s (the Runs page asks more often while runs wait): `/health`'s
 * counts are not enough, since a read-only server reports none for a queue it can read. Hidden
 * without a queue.
 */
export function QueueStatus() {
  const queue = useQueue({ pollMs: QUEUE_POLL_IDLE_MS })
  const [now, setNow] = useState(() => Date.now())
  const waiting = (queue.data?.queued.length ?? 0) > 0
  useEffect(() => {
    if (!waiting) return
    const timer = setInterval(() => setNow(Date.now()), TICK_MS)
    return () => clearInterval(timer)
  }, [waiting])
  const status = queue.data ? queueStatus(queue.data, Math.max(now, queue.dataUpdatedAt)) : null
  return <QueueStatusView status={status} />
}

/** The line for a queue in brief; nothing for no queue. */
export function QueueStatusView({ status }: { status: Status | null }) {
  if (!status) return null
  const label = `Queue: ${queueStatusLabel(status)}`
  const detail = [`${status.running} running`, status.oldestWaitSeconds !== null ? `oldest ${waitLabel(status.oldestWaitSeconds)}` : null, status.draining ? 'draining' : status.paused ? 'paused' : null]
    .filter(Boolean).join(' · ')
  return (
    <Link className="barca-nav barca-queue-status" to="/runs" title={`${label}. Open Runs`} aria-label={label}>
      <StatusGlyph glyph="queued" size={12} />
      <span className="barca-nav-label">
        <span>Queue · {status.waiting} waiting</span>
        <span className="barca-queue-status-detail">{detail}</span>
      </span>
    </Link>
  )
}
