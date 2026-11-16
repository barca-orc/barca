/**
 * Data-health signals (`NodeState.signals`, `GET /state`) in words — pure.
 *
 * The server raises a signal only from facts it recorded (row counts its workers counted, when
 * each result was written); this module says what each one means, one line each, so a list,
 * a panel or a tooltip phrase it the same way.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import type { DataSignal } from './types'

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
