/**
 * What one box of the UI says about a node: its resting cache state, the run working on it, a
 * group's summary, or that its state is not loaded. Every variant carries the tone, the word and
 * the hint a table cell, a graph node or a badge draws; the cache variant also carries the state
 * itself and whether the last attempt failed, which are shown as two facts.
 */

import { cacheStateTone, lastAttemptFailed, type CacheState } from './cacheStateDisplay'
import { statusMeta } from './status'
import { runningGraphState } from './running'
import type { Tone } from './tone'
import type { NodeState, StatusKind } from './types'

export type GraphState =
  /** The node's cache state, as `GET /state` reports it. */
  | { kind: 'cache'; tone: Tone; label: CacheState; hint: string; state: CacheState; lastFailed: boolean }
  /** A run working on the node now, or the run this page streams. */
  | { kind: 'run'; tone: Tone; label: string; hint: string; status: StatusKind }
  /** A group of nodes: its most urgent fact. */
  | { kind: 'group'; tone: Tone; label: string; hint: string }
  /** No state for it: not loaded yet, or the state request failed. */
  | { kind: 'none'; tone: Tone; label: string; hint: string }

export const UNKNOWN_GRAPH_STATE: GraphState = { kind: 'none', tone: 'neutral', label: 'State unavailable', hint: 'Asset state has not been loaded.' }
export const PENDING_GRAPH_STATE: GraphState = { kind: 'none', tone: 'neutral', label: 'Checking…', hint: 'Cache state is being checked.' }

/** A node's resting state: `cache.state` as the server says it, with the server's `detail`. */
export function cacheGraphState(node: NodeState): GraphState {
  const state = node.cache.state
  return { kind: 'cache', tone: cacheStateTone(state), label: state, hint: node.cache.detail, state, lastFailed: lastAttemptFailed(node) }
}

/** A run working on the node now (whoever started it) outranks its resting state. */
export function graphState(node: NodeState): GraphState {
  if (node.running) return runningGraphState(node.running)
  return cacheGraphState(node)
}

/** A run streaming to this page: its status for the node, with the run's own words. */
export function liveGraphState(status: StatusKind): GraphState {
  const meta = statusMeta(status)
  return { kind: 'run', tone: meta.tone, label: meta.label, hint: 'Current run', status }
}

/** Whether the box shows run work: the run's colours and pulse, not a resting state. */
export function isLive(state: GraphState): boolean {
  return state.kind === 'run' && (state.status === 'running' || state.status === 'queued')
}

/** A node's box from its state, or the not-loaded box when the state is missing. */
export function memberHealth(node: NodeState | undefined): GraphState {
  return node ? graphState(node) : UNKNOWN_GRAPH_STATE
}
