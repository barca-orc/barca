/**
 * How a state is drawn: one small glyph (components/StatusGlyph.tsx) per meaning, its shape and
 * its hue. A tone (`tone.ts`) says how urgent something is, and is what logic reads (what
 * needs attention, what sorts first); a glyph is what the eye reads, so states that share a tone
 * (stale, blocked, partial, unsynced are all warnings) still look different. Colour appears on
 * the glyph and nowhere else: a row, a banner or a word around it stays neutral.
 */

import type { Tone } from './tone'

export type Glyph =
  /** Cached, or a run step that succeeded: a filled green circle with a check. */
  | 'fresh'
  /** Its last attempt failed: a filled red circle with a cross. */
  | 'failed'
  /** A run is working on it: a blue ring with an arc that turns while live. */
  | 'running'
  /** Waiting for a worker: a grey ring with clock hands. */
  | 'queued'
  /** Inputs or code changed since its result: an amber ring, half filled. */
  | 'stale'
  /** Kept from running (a failure upstream, or the cache state): an orange ring with a bar. */
  | 'blocked'
  /** Some partition keys cached, not all: a violet ring, three quarters filled. */
  | 'partial'
  /** Cached here, not in the store: a teal ring with an arrow up. */
  | 'unsynced'
  /** No state could be read: a grey ring with a question mark. */
  | 'unknown'
  /** Never materialized: a dashed grey ring. */
  | 'never_run'
  /** Never cached by design (a task, `always`): a grey ring with a turning arrow. */
  | 'always'
  /** Pinned to a version (`barca pin`): an indigo ring with a pin head. */
  | 'pinned'
  /** Its schedule is paused: a grey ring with two bars. */
  | 'paused'
  /** A run stopped by the operator or a shutdown: a grey ring with a square. */
  | 'cancelled'
  /** A step the run did not reach: a grey ring with a dash. */
  | 'skipped'
  /** A step served from cache: a grey ring with a check. */
  | 'cached'
  /** Its state is not loaded yet: a faint dotted ring. */
  | 'pending'

/** Every glyph, in the order the kit and the legend show them. */
export const GLYPHS: readonly Glyph[] = [
  'failed',
  'running',
  'queued',
  'blocked',
  'stale',
  'partial',
  'unsynced',
  'pinned',
  'paused',
  'unknown',
  'never_run',
  'always',
  'fresh',
  'cached',
  'cancelled',
  'skipped',
  'pending',
]

const COLOR = {
  fresh: 'var(--state-fresh)',
  failed: 'var(--state-failed)',
  running: 'var(--state-running)',
  queued: 'var(--state-idle)',
  stale: 'var(--state-stale)',
  blocked: 'var(--state-blocked)',
  partial: 'var(--state-partial)',
  unsynced: 'var(--state-unsynced)',
  unknown: 'var(--state-idle)',
  never_run: 'var(--state-idle)',
  always: 'var(--state-idle)',
  pinned: 'var(--state-pinned)',
  paused: 'var(--state-idle)',
  cancelled: 'var(--state-idle)',
  skipped: 'var(--state-idle)',
  cached: 'var(--state-idle)',
  pending: 'var(--state-idle)',
} satisfies Record<Glyph, string>

/** The CSS colour of a glyph: what a bar segment or a minimap node in that state is filled with. */
export function glyphColor(glyph: Glyph): string {
  return COLOR[glyph]
}

/** A glyph for a bare tone, where nothing more is known (a group's summary, a sidebar row). */
export function toneGlyph(tone: Tone): Glyph {
  switch (tone) {
    case 'success':
      return 'fresh'
    case 'failed':
      return 'failed'
    case 'running':
      return 'running'
    case 'queued':
      return 'queued'
    case 'warning':
      return 'stale'
    case 'neutral':
      return 'pending'
  }
}
