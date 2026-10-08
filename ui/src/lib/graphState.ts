import { match } from 'ts-pattern'
import { severityOf } from './assetTable'
import { statusMeta } from './status'
import type { NodeState, StatusKind } from './types'

export interface GraphState { status: StatusKind; label: string; hint: string }
export const UNKNOWN_GRAPH_STATE: GraphState = { status: 'skipped', label: 'State unavailable', hint: 'Asset state has not been loaded.' }

/** Resting colors describe cache state; a failed latest attempt takes priority. */
export function graphState(node: NodeState): GraphState {
  const state = match(severityOf(node))
    .with('failed', () => ({ status: 'failed' as const, label: 'Last attempt failed' }))
    .with('cached', () => ({ status: 'success' as const, label: 'Cached' }))
    .with('stale', () => ({ status: 'warning' as const, label: 'Stale' }))
    .with('partial', () => ({ status: 'warning' as const, label: 'Partial cache' }))
    .with('never_run', () => ({ status: 'skipped' as const, label: 'Never run' }))
    .with('always_runs', () => ({ status: 'skipped' as const, label: 'Always runs' }))
    .with('unknown', () => ({ status: 'skipped' as const, label: 'State unknown' }))
    .exhaustive()
  return { ...state, hint: state.status === 'failed' ? node.last_materialization?.error ?? state.label : node.cache.detail }
}

/** Live overlays are separate from resting cache state and keep their own labels. */
export function liveGraphState(status: StatusKind): GraphState {
  return { status, label: statusMeta(status).label, hint: 'Current run' }
}

