import { match } from 'ts-pattern'
import type { StatusKind, ListNode, NodeKind } from './types'
import type { Severity } from './assetTable'

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
      label: 'Healthy',
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
      label: 'Stale',
      live: false,
    }))
    .with('queued', 'pending', () => ({
      color: 'var(--status-queued)',
      bg: 'var(--status-queued-bg)',
      line: 'var(--status-queued-line)',
      label: 'Running · queued',
      live: false,
    }))
    .with('skipped', () => ({
      color: 'var(--status-skipped)',
      bg: 'var(--status-skipped-bg)',
      line: 'transparent',
      label: 'Not run',
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

/**
 * A task that checks data. The graph has no such kind: the name says so, by the `check__` and
 * `validate__` conventions.
 */
export function isCheck(node: { id: string; kind: NodeKind | 'group' }): boolean {
  return node.kind === 'task' && /(^|:)(check|validate)__/.test(node.id)
}

/** Short label for a node kind: `check` for a check, wherever a node is shown. */
export function nodeKindLabel(node: { id: string; kind: NodeKind }): string {
  return match(node.kind)
    .with('asset', () => 'asset')
    .with('sensor', () => 'sensor')
    .with('task', () => (isCheck(node) ? 'check' : 'task'))
    .exhaustive()
}

/** One name per state, in lower case for counts and filters (`3 failed`, `12 not run`). */
export function severityWord(severity: Severity): string {
  return match(severity)
    .with('failed', () => 'failed')
    .with('stale', () => 'stale')
    .with('never_run', () => 'not run')
    .with('partial', () => 'stale (some keys)')
    .with('unknown', () => 'unknown')
    .with('always_runs', () => 'always runs')
    .with('unsynced', () => 'unsynced')
    .with('cached', () => 'healthy')
    .exhaustive()
}
