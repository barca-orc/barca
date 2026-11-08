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
  /** The state, as the CLI prints it (`barca status --json`). */
  word: CacheState
  /** What the UI says for it: plain words, never the snake_case enum. */
  label: string
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
  blocked: { word: 'blocked', label: 'blocked', tone: 'warning' },
  stale: { word: 'stale', label: 'stale', tone: 'warning' },
  partial: { word: 'partial', label: 'partial', tone: 'warning' },
  unsynced: { word: 'unsynced', label: 'unsynced', tone: 'warning' },
  unknown: { word: 'unknown', label: 'unknown', tone: 'neutral' },
  never_run: { word: 'never_run', label: 'never run', tone: 'neutral' },
  always_runs: { word: 'always_runs', label: 'always runs', tone: 'neutral' },
  cached: { word: 'cached', label: 'fresh', tone: 'success' },
} satisfies Record<CacheState, CacheStateDisplay>

export function cacheStateDisplay(state: CacheState): CacheStateDisplay {
  return DISPLAY[state]
}

/** The words the UI shows for a state: `never run`, `fresh`. */
export function cacheStateLabel(state: CacheState): string {
  return DISPLAY[state].label
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

/** The word of that fact: `1 failing`. It replaces the state's word wherever a node is shown. */
export const LAST_ATTEMPT_FAILED = 'failing'

/**
 * The word of the fact "a failed node upstream keeps it from running": `blocked`, in the warning
 * tone with its own icon. It is derived on the client from the graph (`blockedBy` in
 * `graphView`), never sent as a cache state; the cache state stays in the tooltip and the panel.
 */
export const BLOCKED_UPSTREAM = 'blocked'

/** How a node's one state is drawn: its words, tone and, when the tone does not say it, an icon. */
export interface NodeStateShown {
  label: string
  tone: Tone
  icon?: 'blocked'
}

/**
 * The one state a node shows: `failing` when its latest attempt failed (it outranks the cache
 * state, which stays in the tooltip and the panel), else `blocked` when a failed node upstream
 * keeps it from running, else its cache state's words.
 */
export function nodeStateDisplay(node: { state: CacheState; lastFailed: boolean; blockedBy?: string | null }): NodeStateShown {
  if (node.lastFailed) return { label: LAST_ATTEMPT_FAILED, tone: FAILED_TONE }
  if (node.blockedBy) return { label: BLOCKED_UPSTREAM, tone: 'warning', icon: 'blocked' }
  return { label: cacheStateLabel(node.state), tone: cacheStateTone(node.state) }
}

/**
 * Whether the node's latest attempt failed, as `barca status --json` records it: its last
 * materialization's status, or (a partitioned node's last materialization is one key) failed
 * keys. A fact beside `cache.state`, never a replacement for it.
 */
export function lastAttemptFailed(node: Pick<NodeState, 'last_materialization' | 'partitions'>): boolean {
  return node.last_materialization?.status === 'failed' || (node.partitions?.failed ?? 0) > 0
}
