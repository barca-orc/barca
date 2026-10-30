import { HEALTH_BUCKETS, healthLabel, type HealthBucket, type HealthCounts } from '@/lib/health'

export interface HealthBarProps {
  counts: HealthCounts
  /** How many buckets the label names, most urgent first (the tooltip names them all). */
  labelLimit?: number
  /** Bar width in px; the label follows it. */
  width?: number
}

const COLOR: Record<HealthBucket, string> = {
  failed: 'var(--status-failed)',
  running: 'var(--status-running)',
  stale: 'var(--status-warning)',
  idle: 'var(--status-skipped)',
  healthy: 'var(--status-success)',
  pending: 'var(--border-default)',
}

/**
 * barca · HealthBar
 * A scope's nodes by state, as one proportional bar and a label naming the most urgent
 * buckets: `2 failed · 121 stale`. A failed or running node always gets a visible sliver,
 * however many healthy nodes surround it.
 */
export function HealthBar({ counts, labelLimit = 2, width = 96 }: HealthBarProps) {
  const total = Math.max(1, counts.total)
  const present = HEALTH_BUCKETS.filter((b) => counts[b] > 0)
  const label = healthLabel(counts, labelLimit) || 'empty'
  const urgent = counts.failed > 0 ? 'failed' : counts.running > 0 ? 'running' : counts.stale > 0 ? 'stale' : undefined
  return (
    <span className="barca-health" title={healthLabel(counts)} data-urgent={urgent}>
      <span className="barca-health-bar" style={{ width }} role="img" aria-label={healthLabel(counts) || 'empty'}>
        {present.map((b) => (
          <span
            key={b}
            data-bucket={b}
            style={{ flexGrow: counts[b] / total, minWidth: b === 'failed' || b === 'running' ? 3 : 1, background: COLOR[b] }}
          />
        ))}
      </span>
      <span className="barca-health-label">{label}</span>
    </span>
  )
}
