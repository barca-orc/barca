import { match } from 'ts-pattern'
import type { StatusKind, ListNode, NodeKind } from './types'

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

/** Human-readable freshness summary: the cron expression for a scheduled node. */
export function freshnessLabel(node: Pick<ListNode, 'freshness' | 'schedule'>): string {
  return match(node.freshness)
    .with('always', () => 'always')
    .with('manual', () => 'manual')
    .with('schedule', () => node.schedule ?? 'schedule')
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
