/**
 * How a cause is shown: the word, tone and colour of each value. Display only. What a node
 * of a cause weighs on the health map is served (`weight` and `weights` of a rollup entry).
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
import type { NoEstimate } from './generated/NoEstimate'
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
  /** What the word means, in a line: the legend's and a tooltip's text. */
  meaning: string
}

const DISPLAY = {
  failing: { label: 'failing', tone: 'failed', glyph: 'failed', alarm: true, meaning: 'It failed, or a failing node upstream holds it.' },
  overdue: { label: 'overdue', tone: 'warning', glyph: 'blocked', alarm: true, meaning: 'Ticks reached it and did not leave it fresh.' },
  no_refresh_path: { label: 'no refresh path', tone: 'warning', glyph: 'stale', alarm: true, meaning: 'Nothing scheduled will ever refresh it.' },
  queued: { label: 'queued', tone: 'queued', glyph: 'queued', alarm: false, meaning: 'What would refresh it waits for a run slot.' },
  undecided: { label: 'undecided', tone: 'neutral', glyph: 'unknown', alarm: false, meaning: 'barca cannot decide yet: a cutoff or sensor upstream has not reported.' },
  catching_up: { label: 'catching up', tone: 'running', glyph: 'always', alarm: false, meaning: 'A run is computing it, or the next tick of its refresh path will.' },
} satisfies Record<Cause, CauseDisplay>

export function causeDisplay(cause: Cause): CauseDisplay {
  return DISPLAY[cause]
}

/** Whether nodes of this cause lead a scope's line even while a run works in it: a failure does. */
export function leadsRunWork(cause: Cause): boolean {
  return cause === 'failing'
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

/** The CSS colour of a map part (`--map-*`, tokens/colors.css). */
export function mapPartColor(part: MapPart): string {
  return `var(--map-${part.replaceAll('_', '-')})`
}

/**
 * The colour of a band of a partitioned node's keys: failed keys are failing, keys with a
 * result fresh, and keys not up to date take the colour of the cause the server gives them
 * (`keys.cause` of the node's rollup entry: why the node is not fresh).
 */
export function keyBandPart(band: 'failed' | 'behind' | 'ok', behind: Cause | null): MapPart {
  return band === 'failed' ? 'failing' : band === 'ok' ? 'fresh' : mapPartOf(behind)
}

/** Where a map part sorts: the served order of the causes (what alarms first), then fresh. */
export function mapPartRank(part: MapPart): number {
  return MAP_PARTS.indexOf(part)
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

/** Why a node has no estimate (`no_estimate`), in the words `barca-core` gives each reason. */
const NO_ESTIMATE = {
  failing: 'until the failure is fixed',
  paused: 'paused',
  scheduler_off: 'no scheduler is firing its schedule',
  no_path: 'no refresh path',
  undecided: 'undecided until a run reports',
  missed: 'ticks missed it',
  queue_stopped: 'the run queue is not starting runs',
  no_scheduler_here: 'no scheduler runs here',
  not_promised: 'a result above it may come out unchanged',
} satisfies Record<NoEstimate, string>

export function noEstimateWord(reason: NoEstimate): string {
  return NO_ESTIMATE[reason]
}

/**
 * The reasons a tick's next look can settle: a row with one of them and a `recheck_at` says
 * when it is rechecked. Any other reason needs someone (or a queue or scheduler that is not
 * there), so the row says the reason, and the recheck only in its tooltip.
 */
const SETTLED_BY_A_TICK: readonly NoEstimate[] = ['not_promised', 'undecided']

export function settledByATick(reason: NoEstimate | null): boolean {
  return reason === null || SETTLED_BY_A_TICK.includes(reason)
}

/** How a row or a bar shows a node's place on the map: its cause, or fresh. */
export function mapPartOf(cause: Cause | null): MapPart {
  return cause ?? 'fresh'
}

/** The glyph of a map part: a cause's, or the fresh one. */
export function mapPartGlyph(part: MapPart): Glyph {
  return part === 'fresh' ? 'fresh' : DISPLAY[part].glyph
}
