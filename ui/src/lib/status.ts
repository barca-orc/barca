import { match } from 'ts-pattern'
import type { StatusKind, RunStatus, Freshness, NodeKind } from './types'

export interface StatusMeta {
  /** CSS var for the status color */
  color: string
  /** CSS var for the faint tinted background */
  bg: string
  /** CSS var for the border line */
  line: string
  /** Human label */
  label: string
  /** Whether the indicator should pulse (live work) */
  live: boolean
}

/** Map a visual status to its design-system color tokens + label. */
export function statusMeta(status: StatusKind): StatusMeta {
  return match<StatusKind, StatusMeta>(status)
    .with('success', () => ({
      color: 'var(--status-success)',
      bg: 'var(--status-success-bg)',
      line: 'var(--status-success-line)',
      label: 'Succeeded',
      live: false,
    }))
    .with('running', () => ({
      color: 'var(--status-running)',
      bg: 'var(--status-running-bg)',
      line: 'var(--status-running-line)',
      label: 'Running',
      live: true,
    }))
    .with('failed', () => ({
      color: 'var(--status-failed)',
      bg: 'var(--status-failed-bg)',
      line: 'var(--status-failed-line)',
      label: 'Failed',
      live: false,
    }))
    .with('warning', () => ({
      color: 'var(--status-warning)',
      bg: 'var(--status-warning-bg)',
      line: 'var(--status-warning-line)',
      label: 'Warning',
      live: false,
    }))
    .with('queued', 'pending', () => ({
      color: 'var(--status-queued)',
      bg: 'var(--status-queued-bg)',
      line: 'var(--status-queued-line)',
      label: 'Queued',
      live: false,
    }))
    .with('skipped', () => ({
      color: 'var(--status-skipped)',
      bg: 'var(--status-skipped-bg)',
      line: 'transparent',
      label: 'Skipped',
      live: false,
    }))
    .exhaustive()
}

/** Collapse the server's run lifecycle into a visual status. */
export function runStatusToVisual(status: RunStatus): StatusKind {
  return match<RunStatus, StatusKind>(status)
    .with('pending', () => 'queued')
    .with('running', () => 'running')
    .with('complete', () => 'success')
    .with('failed', () => 'failed')
    // Stopped by the operator (DELETE /run/{id}) or server shutdown — inert,
    // not an error.
    .with('cancelled', () => 'skipped')
    .exhaustive()
}

/** Human-readable freshness summary. */
export function freshnessLabel(freshness: Freshness): string {
  return match(freshness)
    .with({ type: 'Always' }, () => 'always')
    .with({ type: 'Manual' }, () => 'manual')
    .with({ type: 'Schedule' }, (f) => f.value)
    .exhaustive()
}

/** Short label for a node kind. */
export function nodeKindLabel(kind: NodeKind): string {
  return match(kind)
    .with('asset', () => 'asset')
    .with('sensor', () => 'sensor')
    .with('task', () => 'task')
    .exhaustive()
}
