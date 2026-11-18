/**
 * What one box of the UI says about a node: where it stands at rest, the run working on it, a
 * group's summary, or that its state is not loaded. Every variant carries the tone, the word and
 * the hint a graph node, a palette line or a badge draws.
 *
 * At rest a node reads as the health map reads it: its served `cause` (`failing`, `overdue`,
 * `no refresh path`, `queued`, `undecided`, `catching up`) or `fresh`, in that word and that
 * colour (`lib/causeDisplay.ts`). Display only: the cause is the server's, and "the last attempt
 * failed" is the server's `health` being `failing` (`lib/healthDisplay.ts`); nothing here
 * decides either.
 */

import { lastAttemptFailed, shownCacheState, type CacheState } from './cacheStateDisplay'
import { causeDisplay, mapPartGlyph, mapPartLabel, mapPartMeaning, mapPartOf, type MapPart } from './causeDisplay'
import { blockerOf } from './healthDisplay'
import { statusMeta } from './status'
import { runningGraphState } from './running'
import type { Tone } from './tone'
import type { Glyph } from './glyph'
import type { NodeState, StatusKind } from './types'

export type GraphState =
  /**
   * The node at rest, as `GET /state` reports it: `part` is its served cause (or `fresh`), which
   * the word, the glyph and the colour are of; `state` is its cache state, kept for the pages
   * that ask about it.
   */
  | { kind: 'cache'; tone: Tone; glyph: Glyph; label: string; hint: string; part: MapPart; state: CacheState; lastFailed: boolean; keys?: GraphKeys; /** The failing node that holds it, as the server names it (`blockerOf`). */ blockedBy?: string }
  /** A run working on the node now, or the run this page streams; for a group, its parts as above. */
  | { kind: 'run'; tone: Tone; glyph: Glyph; label: string; hint: string; status: StatusKind; parts?: readonly GroupPart[] }
  /**
   * A group of nodes: its leading cause (`label`, `glyph`, `part`), and every part of its
   * members, in the served order of the causes, for a card to show the rest compactly.
   */
  | { kind: 'group'; tone: Tone; glyph: Glyph; label: string; hint: string; part?: MapPart; parts?: readonly GroupPart[] }
  /** No state for it: not loaded yet, or the state request failed. */
  | { kind: 'none'; tone: Tone; glyph: Glyph; label: string; hint: string }

/** One part of a group: `2 failing`, `1 running`, `48 catching up`, `120 fresh`, with its glyph. */
export interface GroupPart {
  glyph: Glyph
  tone: Tone
  /** The cause (or fresh) the part counts, whose colour it is drawn in; absent for run work. */
  part?: MapPart
  /** How many of its members. */
  count: number
  /** The whole part in words: `2 failing`. */
  text: string
  /** The state's word alone: `failing`. */
  word: string
}

export const UNKNOWN_GRAPH_STATE: GraphState = { kind: 'none', tone: 'neutral', glyph: 'unknown', label: 'State unavailable', hint: 'Asset state has not been loaded.' }
export const PENDING_GRAPH_STATE: GraphState = { kind: 'none', tone: 'neutral', glyph: 'pending', label: 'Checking…', hint: 'Cache state is being checked.' }

/** A partitioned node's keys: how many, how many last failed, how many wait to run (stale or blocked). */
export interface GraphKeys {
  total: number
  failed: number
  waiting: number
}

/** The tone of a map part: a cause's, or success for fresh. */
function partTone(part: MapPart): Tone {
  return part === 'fresh' ? 'success' : causeDisplay(part).tone
}

/**
 * A node at rest: its served `cause` in the health map's word, or `fresh`; the hint is what the
 * word means, then the server's own account of the node's cache state (`cache.detail`).
 */
export function cacheGraphState(node: NodeState): GraphState {
  const part = mapPartOf(node.cause)
  const p = node.partitions
  const keys = p ? { total: p.total, failed: p.failed ?? 0, waiting: Math.max(0, p.missing - (p.failed ?? 0)) } : undefined
  const blockedBy = blockerOf(node)
  const hint = [mapPartMeaning(part), node.cache.detail].filter(Boolean).join(' ')
  return { kind: 'cache', tone: partTone(part), glyph: mapPartGlyph(part), label: mapPartLabel(part), hint, part, state: shownCacheState(node), lastFailed: lastAttemptFailed(node), ...(keys ? { keys } : {}), ...(blockedBy ? { blockedBy } : {}) }
}

/** A run working on the node now (whoever started it) outranks its resting state. */
export function graphState(node: NodeState): GraphState {
  if (node.running) return runningGraphState(node.running)
  return cacheGraphState(node)
}

/** A run streaming to this page: its status for the node, with the run's own words. */
export function liveGraphState(status: StatusKind): GraphState {
  const meta = statusMeta(status)
  return { kind: 'run', tone: meta.tone, glyph: meta.glyph, label: meta.label, hint: 'Current run', status }
}

/** Whether the box shows run work: the run's colours and pulse, not a resting state. */
export function isLive(state: GraphState): boolean {
  return state.kind === 'run' && (state.status === 'running' || state.status === 'queued')
}

/** A node's box from its state, or the not-loaded box when the state is missing. */
export function memberHealth(node: NodeState | undefined): GraphState {
  return node ? graphState(node) : UNKNOWN_GRAPH_STATE
}
