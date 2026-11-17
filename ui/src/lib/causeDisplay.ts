/**
 * How a cause is shown: the word, tone, colour and weight of each value. Display only.
 *
 * Why a node is not fresh is decided in `barca-core` (`specs/behavior/health.md`, "Cause") and
 * arrives as `cause` on every row and as counts on every rollup entry (`causes`). Nothing here
 * reads two fields to produce a third. The values, in the server's order (what alarms first,
 * the order `barca status --rollup` prints them in):
 *
 * - `failing`: it failed, or a failing node upstream holds it. Alarms, in its own tone.
 * - `overdue`: ticks reached it and did not leave it fresh. Alarms.
 * - `no_refresh_path`: nothing scheduled will ever refresh it. Alarms.
 * - `queued`: what would refresh it waits for a run slot. Progress, not a warning.
 * - `undecided`: barca cannot decide yet. The one grey.
 * - `catching_up`: a run is computing it, or the next tick will. Progress, not a warning.
 */

import type { Cause } from './generated/Cause'
import type { CauseCounts } from './generated/CauseCounts'
import type { Glyph } from './glyph'
import type { Tone } from './tone'

export type { Cause, CauseCounts }

/** The server's order: what alarms first. */
export const CAUSE_ORDER: readonly Cause[] = ['failing', 'overdue', 'no_refresh_path', 'queued', 'undecided', 'catching_up']

interface CauseDisplay {
  label: string
  tone: Tone
  glyph: Glyph
  /** Whether it needs someone: `failing`, `overdue` and `no_refresh_path` do, nothing else. */
  alarm: boolean
  /**
   * How much room one node with this cause takes on the health map, against 1 for a fresh
   * node: a node that needs someone counts 6, one that will be fresh without anyone acting or
   * that barca cannot decide counts 2. A display scale, like a tone: see `mapWeight`.
   */
  weight: number
  /** What the word means, in a line: the legend's and a tooltip's text. */
  meaning: string
}

const DISPLAY = {
  failing: { label: 'failing', tone: 'failed', glyph: 'failed', alarm: true, weight: 6, meaning: 'It failed, or a failing node upstream holds it.' },
  overdue: { label: 'overdue', tone: 'warning', glyph: 'blocked', alarm: true, weight: 6, meaning: 'Ticks reached it and did not leave it fresh.' },
  no_refresh_path: { label: 'no refresh path', tone: 'warning', glyph: 'stale', alarm: true, weight: 6, meaning: 'Nothing scheduled will ever refresh it.' },
  queued: { label: 'queued', tone: 'queued', glyph: 'queued', alarm: false, weight: 2, meaning: 'What would refresh it waits for a run slot.' },
  undecided: { label: 'undecided', tone: 'neutral', glyph: 'unknown', alarm: false, weight: 2, meaning: 'barca cannot decide yet: a cutoff or sensor upstream has not reported.' },
  catching_up: { label: 'catching up', tone: 'running', glyph: 'always', alarm: false, weight: 2, meaning: 'A run is computing it, or the next tick of its refresh path will.' },
} satisfies Record<Cause, CauseDisplay>

export function causeDisplay(cause: Cause): CauseDisplay {
  return DISPLAY[cause]
}

/** What a segment of the health map is coloured as: a cause, or fresh (no cause). */
export type MapPart = Cause | 'fresh'

/** The order a segment's colours are stacked in: the causes as served, then what is fresh. */
export const MAP_PARTS: readonly MapPart[] = [...CAUSE_ORDER, 'fresh']

/** The word of a map colour: a cause's, or `fresh`. */
export function mapPartLabel(part: MapPart): string {
  return part === 'fresh' ? 'fresh' : DISPLAY[part].label
}

/** What a map colour means, in a line. */
export function mapPartMeaning(part: MapPart): string {
  return part === 'fresh' ? 'Up to date, or its last run succeeded.' : DISPLAY[part].meaning
}

/** The room one node of a colour takes on the map: a cause's weight, 1 for a fresh node. */
export function mapPartWeight(part: MapPart): number {
  return part === 'fresh' ? 1 : DISPLAY[part].weight
}

/** The CSS colour of a map part (`--map-*`, tokens/colors.css). */
export function mapPartColor(part: MapPart): string {
  return `var(--map-${part.replaceAll('_', '-')})`
}

/**
 * The colour of a band of a partitioned node's keys: failed keys are failing, keys with a
 * result fresh, and keys not up to date take the node's own colour (why the node is not fresh).
 */
export function keyBandPart(band: 'failed' | 'behind' | 'ok', own: MapPart): MapPart {
  return band === 'failed' ? 'failing' : band === 'ok' ? 'fresh' : own
}

/** The first cause, in the served order, that `counts` has a node in; `fresh` when it has none. */
export function leadingPart(counts: CauseCounts): MapPart {
  return CAUSE_ORDER.find((c) => counts[c] > 0) ?? 'fresh'
}

/** A cause's count in words: what a legend, the map's centre and a root's line show. */
export interface CausePart {
  cause: Cause
  count: number
  label: string
  alarm: boolean
}

/** Every cause `counts` has a node in, in the served order. */
export function causeParts(counts: CauseCounts): CausePart[] {
  return CAUSE_ORDER.filter((c) => counts[c] > 0).map((c) => ({ cause: c, count: counts[c], label: DISPLAY[c].label, alarm: DISPLAY[c].alarm }))
}

/** `14 overdue · 152 no refresh path · 180 catching up`: the line `barca status --rollup` prints. */
export function causeLine(counts: CauseCounts): string {
  return causeParts(counts).map((p) => `${p.count.toLocaleString()} ${p.label}`).join(' · ')
}
