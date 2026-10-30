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
  /** How many nodes downstream of it, transitively, are waiting on it. */
  impact: number
  /** For a partitioned node: how many keys have no cached result. */
  missingKeys: number | null
}

/** The first line of an error, without the traceback barca records after it. */
export function headline(error: string | null | undefined): string {
  const line = (error ?? '').split('\n').find((l) => l.trim() !== '')
  return line?.trim() ?? 'failed, with no error recorded'
}

/**
 * The failed nodes among `nodes`: the most downstream impact first, then the most recent. A
 * check that failed on a cached asset is a failure too; a node merely stale because something
 * upstream failed is not (it is what `impact` counts).
 */
export function failures(nodes: readonly NodeState[], consumers: ReadonlyMap<string, string[]>): Failure[] {
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
        impact: downstreamIds(n.id, consumers).size,
        missingKeys: n.partitions ? n.partitions.missing : null,
      }
    })
    .sort((a, b) => b.impact - a.impact || (b.at ?? '').localeCompare(a.at ?? '') || a.name.localeCompare(b.name))
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
