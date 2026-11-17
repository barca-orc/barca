/**
 * How a node's health is shown: the word, tone and glyph of each value. Display only.
 *
 * Health itself is classified in `barca-core` (`specs/behavior/health.md`) and arrives on every
 * row as `health`, with the rule that decided it in `health_reason`: the same word `barca
 * status --json` and `barca.nodes` carry. Nothing here reads two fields to produce a third. The
 * words, most severe first:
 *
 * - `failing`: its own last attempt failed. A root cause.
 * - `blocked`: something upstream keeps it from being brought up to date.
 * - `suspect`: its last run succeeded but its recorded row counts look wrong.
 * - `behind`: out of date against its inputs, partly computed, out of sync, or late.
 * - `undecided`: barca cannot decide yet (a cutoff or sensor upstream has not reported).
 * - `never_run`: nothing recorded.
 * - `ok`: fresh, or a task or sensor whose last run succeeded.
 */

import type { Health } from './generated/Health'
import type { Glyph } from './glyph'
import type { Tone } from './tone'
import type { NodeState } from './types'

export type { Health }

/** Most severe first: what sorts first, what a scope's colour is. */
export const HEALTH_ORDER: readonly Health[] = ['failing', 'blocked', 'suspect', 'behind', 'undecided', 'never_run', 'ok']

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
  blocked: { label: 'blocked', tone: 'warning', glyph: 'blocked', meaning: 'Something upstream keeps it from being brought up to date.' },
  suspect: { label: 'suspect data', tone: 'warning', glyph: 'partial', meaning: 'Its last run succeeded, but its recorded row counts look wrong.' },
  behind: { label: 'behind', tone: 'warning', glyph: 'stale', meaning: 'Out of date against its inputs, partly computed, or late.' },
  undecided: { label: 'undecided', tone: 'neutral', glyph: 'unknown', meaning: 'barca cannot decide yet: a cutoff or sensor upstream has not reported.' },
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
  undecided: 'unknown',
  never_run: 'unknown',
  ok: 'ok',
} satisfies Record<Health, HealthFamily>

/** The colour a health is drawn in on the health map. */
export function healthFamily(health: Health): HealthFamily {
  return FAMILY[health]
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
