/**
 * The Failures view as a program: failures grouped by cause, and what a row can do about one.
 * Muting is this browser's alone (localStorage): no server state, and a node that recovers and
 * fails again comes back, because the mute is of one spell of failing.
 */

import { match } from 'ts-pattern'
import { failureGroups, type Failure, type FailureGroup } from './health'
import { only, type Next } from './program'
import type { NodeKind } from './types'

export const MUTED_KEY = 'barca.muted-failures'

export interface FailuresModel {
  muted: readonly string[]
  showMuted: boolean
  /** The last action's outcome. */
  notice: { text: string; failed: boolean } | null
}

export const INITIAL_FAILURES: FailuresModel = { muted: [], showMuted: false, notice: null }

export type FailuresMsg =
  | { type: 'retry'; id: string; kind: NodeKind; name: string }
  | { type: 'copy'; error: string }
  | { type: 'open_run'; id: string; name: string }
  | { type: 'toggle_mute'; key: string }
  | { type: 'toggle_show_muted' }
  | { type: 'done'; text: string }
  | { type: 'action_failed'; text: string }

export type FailuresEffect =
  | { type: 'retry'; id: string; kind: NodeKind; name: string }
  | { type: 'copy'; text: string }
  | { type: 'open_run'; id: string; name: string }
  | { type: 'save_muted'; muted: readonly string[] }

export function updateFailures(model: FailuresModel, msg: FailuresMsg): Next<FailuresModel, FailuresEffect> {
  return match(msg)
    .returnType<Next<FailuresModel, FailuresEffect>>()
    .with({ type: 'retry' }, ({ id, kind, name }) => [{ ...model, notice: null }, [{ type: 'retry', id, kind, name }]])
    .with({ type: 'copy' }, ({ error }) => [{ ...model, notice: null }, [{ type: 'copy', text: error }]])
    .with({ type: 'open_run' }, ({ id, name }) => [{ ...model, notice: null }, [{ type: 'open_run', id, name }]])
    .with({ type: 'toggle_mute' }, ({ key }) => {
      const muted = model.muted.includes(key) ? model.muted.filter((k) => k !== key) : [...model.muted, key]
      return [{ ...model, muted }, [{ type: 'save_muted', muted }]]
    })
    .with({ type: 'toggle_show_muted' }, () => only({ ...model, showMuted: !model.showMuted }))
    .with({ type: 'done' }, ({ text }) => only({ ...model, notice: { text, failed: false } }))
    .with({ type: 'action_failed' }, ({ text }) => only({ ...model, notice: { text, failed: true } }))
    .exhaustive()
}

/** What a mute covers: this node's current spell of failing. */
export function muteKey(f: Pick<Failure, 'id' | 'failingSince' | 'at'>): string {
  return `${f.id}@${f.failingSince ?? f.at ?? ''}`
}

/** The stored list; anything else in storage reads as nothing muted. */
export function parseMuted(raw: string | null): string[] {
  try {
    const value: unknown = JSON.parse(raw ?? '[]')
    return Array.isArray(value) ? value.filter((k): k is string => typeof k === 'string') : []
  } catch {
    return []
  }
}

export type FailureRow =
  | { kind: 'cause'; group: FailureGroup }
  | { kind: 'failure'; failure: Failure; muted: boolean }

export interface FailureTable {
  rows: FailureRow[]
  mutedCount: number
}

/** The table's rows, one heading per cause; muted failures are left out unless asked for. */
export function failureTable(list: readonly Failure[], model: FailuresModel): FailureTable {
  const muted = new Set(model.muted)
  const isMuted = (f: Failure) => muted.has(muteKey(f))
  const shown = model.showMuted ? list : list.filter((f) => !isMuted(f))
  return {
    rows: failureGroups(shown).flatMap((group): FailureRow[] => [
      { kind: 'cause', group },
      ...group.failures.map((failure): FailureRow => ({ kind: 'failure', failure, muted: isMuted(failure) })),
    ]),
    mutedCount: list.filter(isMuted).length,
  }
}
