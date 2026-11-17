/**
 * Data-health signals (`NodeState.signals`, `GET /state`) in words — pure.
 *
 * The server raises a signal only from facts it recorded (row counts its workers counted, when
 * each result was written); this module says what each one means, one line each, so a list,
 * a panel or a tooltip phrase it the same way.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import { formatLate } from './schedules'
import { parseTime } from './time'
import type { DataSignal, NodeState } from './types'

const n = (count: number) => count.toLocaleString('en-US')
const rows = (count: number) => `${n(count)} ${count === 1 ? 'row' : 'rows'}`

/** "3 of 1,205 keys empty, previously 0"; "empty output"; "rows 1,200 → 300"; "behind orders". */
export function signalLine(signal: DataSignal): string {
  return match(signal)
    .with({ kind: 'empty' }, (s) => {
      const before = s.previous_empty_keys === null ? '' : `, previously ${n(s.previous_empty_keys)}`
      return s.keys === 1 && s.empty_keys === 1
        ? `empty output${s.previous_empty_keys === 0 ? ' (had rows before)' : ''}`
        : `${n(s.empty_keys)} of ${n(s.keys)} keys empty${before}`
    })
    .with({ kind: 'rows_changed' }, (s) => {
      const change = `${rows(s.previous_rows)} → ${n(s.rows)}`
      if (s.key === null) return `rows changed: ${change}`
      const others = s.changed_keys > 1 ? ` (${n(s.changed_keys)} keys changed)` : ''
      return `rows changed on ${s.key}: ${change}${others}`
    })
    .with({ kind: 'behind_input' }, (s) => `older than its input ${shortName(s.input)}`)
    .exhaustive()
}

export interface DataSignalSets {
  /** Nodes whose output looks wrong (empty, or rows sharply changed), with what is wrong. */
  suspect: Map<string, string>
  /** Nodes that are behind: older than an input, or a schedule that missed its slot. */
  behind: Map<string, string>
}

/** The signals of `states` as the causes list words them, by node: what is suspect, what is behind. */
export function dataSignalSets(states: readonly NodeState[], nowMs: number = Date.now()): DataSignalSets {
  const suspect = new Map<string, string>()
  const behind = new Map<string, string>()
  for (const s of states) {
    const signals = s.signals ?? []
    const data = signals.filter((x) => x.kind === 'empty' || x.kind === 'rows_changed').map(signalLine)
    if (data.length > 0) suspect.set(s.id, data.join(' · '))
    const older = signals.find((x) => x.kind === 'behind_input')
    if (older) behind.set(s.id, signalLine(older))
    else if (s.schedule?.late && s.schedule.late_since) {
      // Late is the server's word (`schedule.late`), the one the Schedules page shows.
      const since = parseTime(s.schedule.late_since)
      if (since !== null) behind.set(s.id, `schedule missed its slot: ${formatLate(since, nowMs)}`)
    }
  }
  return { suspect, behind }
}
