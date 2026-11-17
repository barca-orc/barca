/**
 * A node's health: one word that says whether something needs doing about it, and what kind of
 * thing. Health is derived from facts the server reports (`GET /state`: the cache state and its
 * reason, the last attempt, partition counts, data signals) and the graph; it never replaces
 * the cache state, which stays in the node's panel. The words, most severe first:
 *
 * - `failing`: its own last attempt failed. A root cause.
 * - `blocked`: something upstream is failing, so it cannot be brought up to date.
 * - `suspect`: its last run succeeded but the output looks wrong (empty, sharply changed, or a
 *   fan-in older than its input).
 * - `behind`: stale against its inputs, partly computed, out of sync, or a schedule that missed
 *   its slot.
 * - `waiting`: barca cannot decide yet (a cutoff or sensor upstream has not reported); expected,
 *   shown calmly.
 * - `never_run`: nothing recorded.
 * - `ok`: fresh, or a task or sensor whose last run succeeded.
 */

import { match } from 'ts-pattern'
import { lastAttemptFailed } from './cacheStateDisplay'
import type { Glyph } from './glyph'
import type { Tone } from './tone'
import type { NodeState } from './types'

export type Health = 'failing' | 'blocked' | 'suspect' | 'behind' | 'waiting' | 'never_run' | 'ok'

/** Most severe first: what sorts first, what a scope's colour is. */
export const HEALTH_ORDER: readonly Health[] = ['failing', 'blocked', 'suspect', 'behind', 'waiting', 'never_run', 'ok']

/** Health that is a problem: what a health map sizes by and a causes list explains. */
export const UNHEALTHY: readonly Health[] = ['failing', 'blocked', 'suspect', 'behind']

interface HealthDisplay {
  label: string
  tone: Tone
  glyph: Glyph
  /** What the word means, in a line: the legend's and a tooltip's text. */
  meaning: string
}

const DISPLAY = {
  failing: { label: 'failing', tone: 'failed', glyph: 'failed', meaning: 'Its own last attempt failed.' },
  blocked: { label: 'blocked', tone: 'warning', glyph: 'blocked', meaning: 'Something upstream is failing, so it cannot be brought up to date.' },
  suspect: { label: 'suspect data', tone: 'warning', glyph: 'partial', meaning: 'Its last run succeeded, but the output looks wrong.' },
  behind: { label: 'behind', tone: 'warning', glyph: 'stale', meaning: 'Out of date against its inputs, partly computed, or late.' },
  waiting: { label: 'waiting', tone: 'neutral', glyph: 'unknown', meaning: 'Waits on an upstream cutoff or sensor to decide; expected.' },
  never_run: { label: 'never run', tone: 'neutral', glyph: 'never_run', meaning: 'Nothing recorded yet.' },
  ok: { label: 'ok', tone: 'success', glyph: 'fresh', meaning: 'Up to date, or its last run succeeded.' },
} satisfies Record<Health, HealthDisplay>

export function healthDisplay(health: Health): HealthDisplay {
  return DISPLAY[health]
}

export function healthRank(health: Health): number {
  return HEALTH_ORDER.indexOf(health)
}

/** The most severe of `healths`; `ok` for none. */
export function worstHealth(healths: Iterable<Health>): Health {
  let worst: Health = 'ok'
  for (const h of healths) if (healthRank(h) < healthRank(worst)) worst = h
  return worst
}

export function isUnhealthy(health: Health): boolean {
  return UNHEALTHY.includes(health)
}

/** The health map's four colours: failing, needs attention, ok, not known yet. */
export type HealthFamily = 'failing' | 'attention' | 'ok' | 'unknown'

const FAMILY = {
  failing: 'failing',
  blocked: 'attention',
  suspect: 'attention',
  behind: 'attention',
  waiting: 'unknown',
  never_run: 'unknown',
  ok: 'ok',
} satisfies Record<Health, HealthFamily>

/** The colour a health is drawn in on the health map. */
export function healthFamily(health: Health): HealthFamily {
  return FAMILY[health]
}

/** Facts beyond the node's own state row that decide its health. */
export interface HealthContext {
  /** A failing node upstream keeps it from running (`blockedUpstream`). */
  blocked: boolean
  /** Its output looks wrong (data signals). */
  suspect: boolean
  /** A schedule that missed its slot. */
  late: boolean
}

const NO_CONTEXT: HealthContext = { blocked: false, suspect: false, late: false }

/** A node's health from its state row and the facts around it. */
export function nodeHealth(node: NodeState, ctx: Partial<HealthContext> = {}): Health {
  const { blocked, suspect, late } = { ...NO_CONTEXT, ...ctx }
  if (lastAttemptFailed(node)) return 'failing'
  if (blocked || node.cache.state === 'blocked' || node.cache.reason === 'upstream_failed') return 'blocked'
  if (suspect) return 'suspect'
  if (late) return 'behind'
  return match(node.cache.state)
    .with('stale', 'partial', 'unsynced', () => 'behind' as const)
    .with('unknown', () => 'waiting' as const)
    .with('never_run', () => 'never_run' as const)
    .with('always_runs', () => (node.last_materialization ? ('ok' as const) : ('never_run' as const)))
    .with('cached', () => 'ok' as const)
    .exhaustive()
}

/**
 * How much a node's health weighs: for a partitioned node, the keys it concerns (failed keys
 * for a failure, keys without a result otherwise); 1 for any other unhealthy node; 0 when
 * healthy. What a health map sizes by, so a node with 1,205 failed keys outweighs a stale one.
 */
export function healthWeight(node: NodeState, health: Health): number {
  if (!isUnhealthy(health)) return 0
  const p = node.partitions
  if (!p) return 1
  if (health === 'failing') return Math.max(1, p.failed ?? 0)
  return Math.max(1, p.missing)
}

/**
 * Whether a node is in a state a failure upstream can keep it in: not up to date, and not
 * failing itself. Used to pass `blocked` down the graph.
 */
function canBeBlocked(node: NodeState | undefined): boolean {
  if (!node || lastAttemptFailed(node)) return false
  return node.cache.state !== 'cached' && node.cache.state !== 'always_runs'
}

/**
 * Each node a failing node upstream keeps from running, with that failing node (the nearest
 * one found): a direct input that is failing, or the cause behind an input that is itself
 * blocked. Unlike `graphView.blockedBy` this passes through `unknown` nodes too: a node that
 * waits on a cutoff below a failure is blocked by the failure. Pinned nodes stop the spread.
 */
export function blockedCauses(
  nodes: ReadonlyMap<string, NodeState>,
  inputs: ReadonlyMap<string, readonly string[]>,
  pinned: { has(id: string): boolean } = new Set<string>(),
): Map<string, string> {
  const cause = new Map<string, string | null>()
  const visiting = new Set<string>()
  const causeOf = (id: string): string | null => {
    if (cause.has(id)) return cause.get(id)!
    if (visiting.has(id)) return null
    visiting.add(id)
    let found: string | null = null
    for (const input of inputs.get(id) ?? []) {
      const up = nodes.get(input)
      if (!up || pinned.has(input)) continue
      if (lastAttemptFailed(up)) found = input
      else if (canBeBlocked(up)) found = causeOf(input)
      if (found) break
    }
    visiting.delete(id)
    cause.set(id, found)
    return found
  }
  const out = new Map<string, string>()
  for (const [id, node] of nodes) {
    if (!canBeBlocked(node) || pinned.has(id)) continue
    const found = causeOf(id)
    if (found) out.set(id, found)
  }
  return out
}
