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
 *
 * One word is shown differently: `always_runs` (a task, a sensor, an `always` asset) has no
 * cache to describe, so it shows how its last attempt went, as the server says it
 * (`last_attempt` of its row): `succeeded`, or `never run` (`ALWAYS_RUNS`; a failed one is
 * `failing` like any node). That it always runs is a property, said in its tooltip (the
 * server's detail, "tasks always re-run").
 */

import type { CacheState } from './generated/CacheState'
import type { LastAttempt } from './generated/LastAttempt'
import type { NodeState } from './types'
import type { Tone } from './tone'
import type { Glyph } from './glyph'

export type { CacheState }

interface CacheStateDisplay {
  /** The state, as the CLI prints it (`barca status --json`). */
  word: CacheState
  /** What the UI says for it: plain words, never the snake_case enum. */
  label: string
  tone: Tone
  /** How it is drawn: the tone says how urgent, the glyph which state (states share tones, never glyphs). */
  glyph: Glyph
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
  blocked: { word: 'blocked', label: 'blocked', tone: 'warning', glyph: 'blocked' },
  stale: { word: 'stale', label: 'stale', tone: 'warning', glyph: 'stale' },
  partial: { word: 'partial', label: 'partial', tone: 'warning', glyph: 'partial' },
  unsynced: { word: 'unsynced', label: 'unsynced', tone: 'warning', glyph: 'unsynced' },
  // `unknown` is the CLI's word: barca cannot say yet, because a cutoff or sensor upstream has
  // not decided. The one word for it on every surface is `undecided` (the health word).
  unknown: { word: 'unknown', label: 'undecided', tone: 'neutral', glyph: 'unknown' },
  never_run: { word: 'never_run', label: 'never run', tone: 'neutral', glyph: 'never_run' },
  always_runs: { word: 'always_runs', label: 'succeeded', tone: 'success', glyph: 'fresh' },
  cached: { word: 'cached', label: 'fresh', tone: 'success', glyph: 'fresh' },
} satisfies Record<CacheState, CacheStateDisplay>

export function cacheStateDisplay(state: CacheState): CacheStateDisplay {
  return DISPLAY[state]
}

/**
 * How a node that always runs is drawn, by its served `last_attempt`: it has no cache to
 * describe, so it shows how its last attempt went. One that never ran is drawn as `never_run`.
 */
const ALWAYS_RUNS: Record<LastAttempt, CacheStateDisplay> = {
  never_run: DISPLAY.never_run,
  success: DISPLAY.always_runs,
  // Never shown as a state: a failed attempt reads `failing` first (`nodeStateDisplay`).
  failed: { word: 'always_runs', label: 'failing', tone: 'failed', glyph: 'failed' },
}

/** How a state is drawn for a node whose last attempt went `lastAttempt`. */
function shown(state: CacheState, lastAttempt: LastAttempt | undefined): CacheStateDisplay {
  return state === 'always_runs' && lastAttempt !== undefined ? ALWAYS_RUNS[lastAttempt] : DISPLAY[state]
}

/** The words the UI shows for a state: `never run`, `fresh`. */
export function cacheStateLabel(state: CacheState): string {
  return DISPLAY[state].label
}

/** The tone of a state: how urgent it is. */
export function cacheStateTone(state: CacheState): Tone {
  return DISPLAY[state].tone
}

/** The glyph of a state: how it is drawn, beside its word, in a node, a bar or the legend. */
export function cacheStateGlyph(state: CacheState): Glyph {
  return DISPLAY[state].glyph
}

/** The tone of the fact "its last attempt failed", wherever that fact is shown. */
export const FAILED_TONE: Tone = 'failed'

/** The word of that fact: `1 failing`. It replaces the state's word wherever a node is shown. */
export const LAST_ATTEMPT_FAILED = 'failing'

/**
 * The word of the fact "a failed node upstream keeps it from running": `blocked`, in the warning
 * tone with the blocked glyph. The server says which node (`blockerOf` in `healthDisplay`: the
 * `root` of a row whose `health` is `blocked`); the cache state stays in the tooltip and the panel.
 */
export const BLOCKED_UPSTREAM = 'blocked'

/**
 * The word of a pinned asset (`barca pin`): every run reads its pinned version and none
 * recomputes it, so its cache state (often `stale`) would mislead. Warning tone, pinned glyph.
 */
export const PINNED = 'pinned'

/** What a pinned node's state means, in a line: in place of its cache state's explanation. */
export const PINNED_HINT = 'Pinned: runs read this version until it is unpinned.'

/** How a node's one state is drawn: its words, its tone (urgency) and its glyph. */
export interface NodeStateShown {
  label: string
  tone: Tone
  glyph: Glyph
}

/**
 * The one state a node shows: `failing` when its latest attempt failed (it outranks the cache
 * state, which stays in the tooltip and the panel), else `pinned` when it is pinned (`barca pin`), else `blocked` when a failed node upstream
 * keeps it from running, else its cache state's words (a node that always runs: its served
 * `last_attempt`'s, see `ALWAYS_RUNS`).
 */
export function nodeStateDisplay(node: { state: CacheState; lastAttempt?: LastAttempt; lastFailed: boolean; blockedBy?: string | null; pinned?: string | null }): NodeStateShown {
  if (node.lastFailed) return { label: LAST_ATTEMPT_FAILED, tone: FAILED_TONE, glyph: 'failed' }
  if (node.pinned != null) return { label: PINNED, tone: 'warning', glyph: 'pinned' }
  if (node.blockedBy) return { label: BLOCKED_UPSTREAM, tone: 'warning', glyph: 'blocked' }
  const { label, tone, glyph } = shown(node.state, node.lastAttempt)
  return { label, tone, glyph }
}

/**
 * Whether the node's latest attempt failed, as `barca status --json` records it: its last
 * materialization's status, or (a partitioned node's last materialization is one key) failed
 * keys. A fact beside `cache.state`, never a replacement for it.
 */
export function lastAttemptFailed(node: Pick<NodeState, 'health'>): boolean {
  return node.health === 'failing'
}
