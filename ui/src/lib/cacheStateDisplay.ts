/**
 * The one place a cache state is mapped to how it is shown.
 *
 * The UI uses the CLI's eight state words (`barca docs status`), exactly as `barca status
 * --json` prints them, and no others: the word shown for a state is the state. This table gives
 * each its tone (colour; tones may be shared, the word never is) and its rank (what needs
 * attention first). It is keyed by the generated `CacheState`, so a state the server adds is a
 * compile error here until it has a row, and `cache.state` is never rewritten on the client: a
 * failed last attempt is a second fact (`lastAttemptFailed`), shown beside the state, not in
 * place of it.
 */

import type { CacheState } from './generated/CacheState'
import type { NodeState } from './types'
import type { Tone } from './tone'

export type { CacheState }

interface CacheStateDisplay {
  /** The word, as the CLI prints it. */
  word: CacheState
  tone: Tone
}

/**
 * Most urgent first: what a table sorts by, what a health label names first, what a group's
 * summary picks. `cached` is last: nothing to do.
 */
export const CACHE_STATES: readonly CacheState[] = [
  'blocked',
  'stale',
  'partial',
  'unsynced',
  'unknown',
  'never_run',
  'always_runs',
  'cached',
]

const DISPLAY = {
  blocked: { word: 'blocked', tone: 'warning' },
  stale: { word: 'stale', tone: 'warning' },
  partial: { word: 'partial', tone: 'warning' },
  unsynced: { word: 'unsynced', tone: 'warning' },
  unknown: { word: 'unknown', tone: 'neutral' },
  never_run: { word: 'never_run', tone: 'neutral' },
  always_runs: { word: 'always_runs', tone: 'neutral' },
  cached: { word: 'cached', tone: 'success' },
} satisfies Record<CacheState, CacheStateDisplay>

export function cacheStateDisplay(state: CacheState): CacheStateDisplay {
  return DISPLAY[state]
}

/** The tone of a state's word, dot, bar segment or node. */
export function cacheStateTone(state: CacheState): Tone {
  return DISPLAY[state].tone
}

/** A state's place in `CACHE_STATES`: lower is more urgent. */
export function cacheStateRank(state: CacheState): number {
  return CACHE_STATES.indexOf(state)
}

/** The state that needs attention most among `states`; null for none. */
export function worstCacheState(states: Iterable<CacheState>): CacheState | null {
  let worst: CacheState | null = null
  for (const state of states) if (worst === null || cacheStateRank(state) < cacheStateRank(worst)) worst = state
  return worst
}

/** The tone of the fact "its last attempt failed", wherever that fact is shown. */
export const FAILED_TONE: Tone = 'failed'

/** The words of that fact: `1 failed last attempt`, `last attempt failed`. */
export const LAST_ATTEMPT_FAILED = 'last attempt failed'

/**
 * Whether the node's latest attempt failed, as `barca status --json` records it: its last
 * materialization's status, or (a partitioned node's last materialization is one key) failed
 * keys. A fact beside `cache.state`, never a replacement for it.
 */
export function lastAttemptFailed(node: Pick<NodeState, 'last_materialization' | 'partitions'>): boolean {
  return node.last_materialization?.status === 'failed' || (node.partitions?.failed ?? 0) > 0
}

