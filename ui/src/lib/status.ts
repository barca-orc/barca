import { match } from 'ts-pattern'
import { toneMeta, type Tone, type ToneMeta } from './tone'
import type { StatusKind, ListNode, NodeKind } from './types'

export interface StatusMeta extends ToneMeta {
  tone: Tone
  /** The word, as a run's status is printed. */
  label: string
  /** Whether the indicator should pulse (live work). */
  live: boolean
}

/**
 * The run lifecycle's one table: each status of a run or of a step, its tone and its word. This
 * axis is a run's, never a node's cache state (`cacheStateDisplay.ts` has that one).
 */
export function statusMeta(status: StatusKind): StatusMeta {
  const [tone, label, live] = match<StatusKind, [Tone, string, boolean]>(status)
    .with('queued', () => ['queued', 'queued', false])
    .with('running', () => ['running', 'running', true])
    .with('success', () => ['success', 'success', false])
    .with('failed', () => ['failed', 'failed', false])
    // Stopped by the operator or a server shutdown: inert, not an error.
    .with('cancelled', () => ['neutral', 'cancelled', false])
    // A step the run did not reach, or served from cache.
    .with('skipped', () => ['neutral', 'skipped', false])
    .exhaustive()
  return { tone, label, live, ...toneMeta(tone) }
}

/** The tone of a run status. */
export function statusTone(status: StatusKind): Tone {
  return statusMeta(status).tone
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
