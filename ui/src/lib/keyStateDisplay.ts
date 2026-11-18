/**
 * How a key's state is shown: its word, glyph and meaning, the order the states are listed in,
 * and the state a character of a served row stands for. Display only: a key's state is decided
 * in `barca-core` (`partition_grid`) and arrives on every row of `GET /assets/{id}/partitions/keys`,
 * in the rows of `.../partitions/overview` and on `.../partitions/key`.
 */

import type { Glyph } from './glyph'

export type KeyState = 'failed' | 'blocked' | 'stale' | 'never_run' | 'pinned' | 'fresh'

/** Every key state, most urgent first: the order of the count chips, the legend and a tooltip. */
export const KEY_STATES: readonly KeyState[] = ['failed', 'blocked', 'stale', 'never_run', 'pinned', 'fresh']

/** The character each state has in a served row of states (`OverviewRow.states`, `PartitionKeyDetail.state`). */
const CODES: Readonly<Record<string, KeyState>> = { f: 'failed', b: 'blocked', s: 'stale', n: 'never_run', p: 'pinned', c: 'fresh' }

/** The state a served character stands for; null for `-` (not a key) or a character this build does not know. */
export function keyStateOfCode(code: string | undefined): KeyState | null {
  return CODES[code ?? '-'] ?? null
}

/** A served state name as a `KeyState`; null for one this build does not know. */
export function keyStateOf(name: string | undefined): KeyState | null {
  return (KEY_STATES as readonly string[]).includes(name ?? '') ? (name as KeyState) : null
}

interface KeyStateDisplay {
  word: string
  glyph: Glyph
  /** What the state means for one key, for a tooltip or the detail. */
  meaning: string
}

const DISPLAY = {
  failed: { word: 'failed', glyph: 'failed', meaning: 'Its latest attempt failed; a run computes it again.' },
  blocked: { word: 'blocked', glyph: 'blocked', meaning: 'It waits on the same key of an input that failed.' },
  stale: { word: 'stale', glyph: 'stale', meaning: 'Its code or inputs changed since it succeeded; a run computes it again.' },
  never_run: { word: 'never run', glyph: 'never_run', meaning: 'It has never run.' },
  pinned: { word: 'pinned', glyph: 'pinned', meaning: 'Held at an operator-chosen version. Runs read this data until the key is unpinned.' },
  fresh: { word: 'fresh', glyph: 'fresh', meaning: 'Served from cache: a successful result matches its code and inputs.' },
} satisfies Record<KeyState, KeyStateDisplay>

export function keyStateDisplay(state: KeyState): KeyStateDisplay {
  return DISPLAY[state]
}

/** The state drawn quietly: a column of mostly these keys is a mark, not a block. */
export const QUIET_KEY_STATE: KeyState = 'fresh'
