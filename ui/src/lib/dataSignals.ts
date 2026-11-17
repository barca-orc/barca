/**
 * Data signals (`signals` on every status row) and data coverage (`measured`) in words — pure.
 *
 * `barca-core` raises a signal only from facts it recorded (row counts its workers counted,
 * when each result was written) and says whether an asset was measured at all; this module
 * says what each one means, one line each, so a list, a panel or a tooltip phrase it the same
 * way.
 */

import { match } from 'ts-pattern'
import { shortName } from './graph'
import type { DataCoverage } from './generated/DataCoverage'
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
  /** Nodes that are behind an input or whose schedule is late, with which. */
  behind: Map<string, string>
}

/**
 * The words of each node's signals, by node, as the causes list shows them: its `empty` and
 * `rows_changed` signals, and what it is behind. Which nodes these are is the server's answer
 * (`signals`, and `health_reason: schedule_late` for a late schedule); this only words them.
 */
export function dataSignalSets(states: readonly NodeState[], nowMs: number = Date.now()): DataSignalSets {
  const suspect = new Map<string, string>()
  const behind = new Map<string, string>()
  for (const s of states) {
    const data = s.signals.filter((x) => x.kind !== 'behind_input').map(signalLine)
    if (data.length > 0) suspect.set(s.id, data.join(' · '))
    const older = s.signals.find((x) => x.kind === 'behind_input')
    if (older) behind.set(s.id, signalLine(older))
    else if (s.health_reason === 'schedule_late') {
      const minutes = s.late_since === null ? null : Math.round((nowMs - s.late_since * 1000) / 60_000)
      const by = minutes === null ? '' : `: late by ${minutes < 60 ? `${minutes}m` : `${Math.floor(minutes / 60)}h ${minutes % 60}m`}`
      behind.set(s.id, `schedule missed its slot${by}`)
    }
  }
  return { suspect, behind }
}

/**
 * How much of `states`' data was measured: the server's `measured` flags and suspect signals,
 * added up for a scope. The same three numbers as `data_coverage` in `barca status --json`;
 * the rollup (issue 955) will carry them per scope and this sum goes.
 */
export function dataCoverage(states: readonly NodeState[]): DataCoverage {
  const out: DataCoverage = { assets: 0, measured: 0, suspect: 0 }
  for (const s of states) {
    if (s.kind !== 'asset') continue
    out.assets += 1
    if (s.measured === true) out.measured += 1
    if (s.signals.some((x) => x.kind !== 'behind_input')) out.suspect += 1
  }
  return out
}

/**
 * `7 of 556 measured`, then `· 2 suspect`: always said with the count it is out of, so an
 * unmeasured asset reads as not measured, never as fine.
 */
export function coverageLine(coverage: DataCoverage): string {
  const measured = `${n(coverage.measured)} of ${n(coverage.assets)} measured`
  return coverage.suspect > 0 ? `${measured} · ${n(coverage.suspect)} suspect` : measured
}
