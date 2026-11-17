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
import type { LastCheck } from './generated/LastCheck'
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

/** What a tick did with a node (`last_check.outcome`), in words: `checked 3m ago, up to date`. */
const CHECK_OUTCOME = {
  rebuilt: 'rebuilt',
  up_to_date: 'up to date',
  failed: 'failed',
  not_reached: 'not reached',
} satisfies Record<LastCheck['outcome'], string>

export function checkOutcomeLabel(outcome: LastCheck['outcome']): string {
  return CHECK_OUTCOME[outcome]
}

export function healthDisplay(health: Health): HealthDisplay {
  return DISPLAY[health]
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
 * The failing node that keeps `node` from being brought up to date, as the server names it: the
 * `root` of a `blocked` row. Null when it is not blocked by a failing node (a failing node's
 * root is itself). `barca-core` walks the graph (`health::blocked_by`); nothing here does.
 */
export function blockerOf(node: Pick<NodeState, 'health' | 'root'>): string | null {
  return node.health === 'blocked' && node.root ? node.root.node : null
}
