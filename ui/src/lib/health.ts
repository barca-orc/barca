/**
 * What is broken, how much it affects, and why: pure answers over `GET /state` rows and the
 * graph from `GET /assets`. The grouped page's health strip and failures list, and the node
 * panel's "Why" section, only render these.
 */

import { match } from 'ts-pattern'
import { memberHealth } from './groups'
import type { GraphState } from './graphState'
import type { NodeState, StatusKind } from './types'

/** A node's place in the graph: what `GET /assets` says, all a lineage needs. */
export interface GraphLink {
  id: string
  inputs: string[]
}

/** How many nodes of a scope are in each state, as the health bar shows them. */
export interface HealthCounts {
  failed: number
  running: number
  /** Stale, partial or not in the store yet: computed, but not for this code and these inputs. */
  stale: number
  /** Never run, always runs, or state unknown: nothing to say is wrong. */
  idle: number
  healthy: number
  /** No state loaded for them yet. */
  pending: number
  total: number
}

export type HealthBucket = Exclude<keyof HealthCounts, 'total'>

/** Most urgent first: the order of the bar's segments and of its label. */
export const HEALTH_BUCKETS: readonly HealthBucket[] = ['failed', 'running', 'stale', 'idle', 'healthy', 'pending']

export function bucketOf(state: GraphState | undefined): HealthBucket {
  if (!state) return 'pending'
  return match<StatusKind, HealthBucket>(state.status)
    .with('failed', () => 'failed')
    .with('running', 'queued', () => 'running')
    .with('warning', () => 'stale')
    .with('success', () => 'healthy')
    .with('skipped', 'pending', () => 'idle')
    .exhaustive()
}

/** Counts of `ids` by bucket; a live run's status (`live`) outranks the polled state. */
export function healthCounts(
  ids: readonly string[],
  byId: ReadonlyMap<string, NodeState>,
  live: Readonly<Record<string, StatusKind>> = {},
): HealthCounts {
  const counts: HealthCounts = { failed: 0, running: 0, stale: 0, idle: 0, healthy: 0, pending: 0, total: ids.length }
  for (const id of ids) {
    const status = live[id]
    const node = byId.get(id)
    const bucket = status ? bucketOf({ status, label: '', hint: '' }) : node ? bucketOf(memberHealth(node)) : 'pending'
    counts[bucket] += 1
  }
  return counts
}

const BUCKET_WORDS: Record<HealthBucket, [string, string]> = {
  failed: ['failed', 'failed'],
  running: ['running', 'running'],
  stale: ['stale', 'stale'],
  idle: ['not run', 'not run'],
  healthy: ['healthy', 'healthy'],
  pending: ['checking', 'checking'],
}

/** Every non-empty bucket, most urgent first, as the words a label shows for it. */
export function healthParts(counts: HealthCounts, limit = HEALTH_BUCKETS.length): { bucket: HealthBucket; text: string }[] {
  return HEALTH_BUCKETS.filter((b) => counts[b] > 0)
    .slice(0, limit)
    .map((bucket) => ({ bucket, text: `${counts[bucket].toLocaleString()} ${BUCKET_WORDS[bucket][counts[bucket] === 1 ? 0 : 1]}` }))
}

/** `3 failed · 121 stale · 380 healthy`. */
export function healthLabel(counts: HealthCounts, limit = HEALTH_BUCKETS.length): string {
  return healthParts(counts, limit).map((p) => p.text).join(' · ')
}

/** Each node's direct consumers. */
export function consumersOf(graph: readonly GraphLink[]): Map<string, string[]> {
  const out = new Map<string, string[]>()
  for (const node of graph)
    for (const input of node.inputs) {
      const list = out.get(input)
      if (list) list.push(node.id)
      else out.set(input, [node.id])
    }
  return out
}

/** Every node downstream of `id`, transitively (not `id` itself). */
export function downstreamIds(id: string, consumers: ReadonlyMap<string, string[]>): Set<string> {
  const seen = new Set<string>()
  const stack = [...(consumers.get(id) ?? [])]
  while (stack.length) {
    const next = stack.pop()!
    if (seen.has(next)) continue
    seen.add(next)
    for (const c of consumers.get(next) ?? []) if (!seen.has(c)) stack.push(c)
  }
  return seen
}

/** A failure, as the failures list shows it. */
export interface Failure {
  id: string
  name: string
  kind: NodeState['kind']
  /** The error's first line: usually `Type: message`. */
  headline: string
  /** The whole recorded error, traceback included. */
  error: string
  /** When the failed attempt was recorded (UTC, `YYYY-MM-DD HH:MM:SS`). */
  at: string | null
  /** The first failed attempt after its last clean run. */
  failingSince: string | null
  /** Runs in a row that failed it. */
  failures: number
  /** How many nodes downstream of it, transitively, are waiting on it; null until the graph is loaded. */
  impact: number | null
  /** For a partitioned node: how many keys have no cached result. */
  missingKeys: number | null
}

/** How late a group is: its members' worst, so one old failure is not averaged away. */
export interface ScopeAge {
  /** The oldest member's last success; null when no member has succeeded. */
  lastSuccessAt: string | null
  failingSince: string | null
  /** The most runs in a row that failed one member. */
  failures: number
}

/** The ages of `ids`, for those with a loaded state. Times are compared as they are recorded. */
export function scopeAge(ids: readonly string[], byId: ReadonlyMap<string, NodeState>): ScopeAge {
  const age: ScopeAge = { lastSuccessAt: null, failingSince: null, failures: 0 }
  const older = (a: string | null, b: string | null) => (a === null || (b !== null && b < a) ? b ?? a : a)
  for (const id of ids) {
    const node = byId.get(id)
    if (!node) continue
    age.lastSuccessAt = older(age.lastSuccessAt, node.last_success_at)
    age.failingSince = older(age.failingSince, node.failing_since)
    age.failures = Math.max(age.failures, node.consecutive_failures)
  }
  return age
}

/** An error's one line: its first, or a bare traceback's last; no frames. */
export function headline(error: string | null | undefined): string {
  const lines = (error ?? '').split('\n').map((l) => l.trim()).filter((l) => l !== '')
  const first = lines[0]
  if (first === undefined) return 'failed, with no error recorded'
  // A bare traceback says what went wrong on its last line, not its first.
  return /^(Traceback \(most recent call last\)|File ")/.test(first) ? lines.at(-1)! : first
}

/**
 * The failed nodes among `nodes`: the most downstream impact first (unknown last), then the most
 * recent. `consumers` is null while the graph is loading: then the impact is unknown, not 0. A
 * check that failed on a cached asset is a failure too; a node merely stale because something
 * upstream failed is not (it is what `impact` counts).
 */
export function failures(nodes: readonly NodeState[], consumers: ReadonlyMap<string, string[]> | null): Failure[] {
  return nodes
    .filter((n) => memberHealth(n).status === 'failed')
    .map((n) => {
      const error = n.last_materialization?.error ?? ''
      return {
        id: n.id,
        name: n.name,
        kind: n.kind,
        headline: headline(error),
        error,
        at: n.last_materialization?.created_at ?? null,
        failingSince: n.failing_since,
        failures: n.consecutive_failures,
        impact: consumers ? downstreamIds(n.id, consumers).size : null,
        missingKeys: n.partitions ? n.partitions.missing : null,
      }
    })
    .sort((a, b) => (b.impact ?? -1) - (a.impact ?? -1) || (b.at ?? '').localeCompare(a.at ?? '') || a.name.localeCompare(b.name))
}

/** One cause: failures whose error differs at most in its numbers. */
export interface FailureGroup {
  /** The headline with its numbers normalised: what the failures share. */
  cause: string
  failures: Failure[]
  /** The members' impacts added up; null when any is unknown. A node downstream of two counts twice. */
  impact: number | null
}

/** `row count fell 64% day over day` and `… 12% …` are one cause. */
export function causeOf(headline: string): string {
  return headline.replace(/\d[\d,._]*\d|\d/g, '#')
}

/** `list` grouped by cause, the most impact first (unknown last), then the most failures. */
export function failureGroups(list: readonly Failure[]): FailureGroup[] {
  const groups = new Map<string, FailureGroup>()
  for (const f of list) {
    const cause = causeOf(f.headline)
    const group = groups.get(cause) ?? { cause, failures: [], impact: 0 }
    group.failures.push(f)
    group.impact = group.impact === null || f.impact === null ? null : group.impact + f.impact
    groups.set(cause, group)
  }
  return [...groups.values()].sort(
    (a, b) => (b.impact ?? -1) - (a.impact ?? -1) || b.failures.length - a.failures.length || a.cause.localeCompare(b.cause),
  )
}

/** A count for a table: `0` for none, `?` while it is not known. */
export function impactLabel(impact: number | null): string {
  return impact === null ? '?' : impact.toLocaleString()
}

/**
 * The failed nodes upstream of `id` (transitively, nearest first): why a stale or never-run
 * node has not been brought up to date. Only nodes with a loaded state can be judged.
 */
export function failedUpstream(
  id: string,
  inputs: ReadonlyMap<string, readonly string[]>,
  byId: ReadonlyMap<string, NodeState>,
): string[] {
  const out: string[] = []
  const seen = new Set<string>([id])
  let frontier = [...(inputs.get(id) ?? [])]
  while (frontier.length) {
    const next: string[] = []
    for (const up of frontier) {
      if (seen.has(up)) continue
      seen.add(up)
      const node = byId.get(up)
      if (node && memberHealth(node).status === 'failed') out.push(up)
      next.push(...(inputs.get(up) ?? []))
    }
    frontier = next
  }
  return out
}
