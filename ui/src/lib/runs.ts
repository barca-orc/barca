import { match } from 'ts-pattern'
import { statusGlyph } from './status'
import type { Glyph } from './glyph'
import { formatAbsolute, type TimeZone } from './time'
import { shortName } from './graph'
import type { ErrorEnvelope, RunNodeSummary, RunOutcome, RunViewStatus, StatusKind, TriggeredBy } from './types'

const RUN_VIEW_STATUSES: readonly RunViewStatus[] = [
  'pending', 'running', 'success', 'failed', 'cancelled', 'error', 'interrupted',
]

/**
 * Narrow a run's wire `status` (a plain string in the generated types) to the
 * known set; null for a status this UI does not know. `complete` and `ran` are
 * older spellings of `success`.
 */
export function parseRunStatus(status: string): RunViewStatus | null {
  if (status === 'complete' || status === 'ran') return 'success'
  return (RUN_VIEW_STATUSES as readonly string[]).includes(status) ? (status as RunViewStatus) : null
}

/**
 * A run's status in the run lifecycle's vocabulary. A status this UI does not know is shown as
 * given (`runLabel`) and coloured as inert.
 */
export function runStatus(status: string): StatusKind {
  const known = parseRunStatus(status)
  if (known === null) return 'skipped'
  return match<RunViewStatus, StatusKind>(known)
    .with('pending', () => 'queued')
    .with('running', () => 'running')
    .with('success', () => 'success')
    .with('failed', 'error', () => 'failed')
    // Stopped by the operator or a server shutdown: inert, not an error.
    .with('cancelled', 'interrupted', () => 'cancelled')
    .exhaustive()
}

/** The glyph of a run's status. */
export function runGlyph(status: string): Glyph {
  return statusGlyph(runStatus(status))
}

/** A recorded step's glyph: `cached` is a step's own status, beside the run lifecycle's. */
export function stepGlyph(status: string): Glyph {
  return status === 'cached' ? 'cached' : runGlyph(status)
}

/** `3,077 keys · 40 failed · 7 calls`. */
export function runNodeLine(n: RunNodeSummary): string {
  const parts = [`${n.keys.toLocaleString()} key${n.keys === 1 ? '' : 's'}`]
  if (n.failed) parts.push(`${n.failed.toLocaleString()} failed`)
  if (n.calls) parts.push(`${n.calls.toLocaleString()} call${n.calls === 1 ? '' : 's'}`)
  return parts.join(' · ')
}

/** Whether the node recorded no step this run: it was cached, or skipped. */
export function runNodeIdle(n: RunNodeSummary): boolean {
  return n.status === 'cached' || n.status === 'skipped'
}

/**
 * A node's badge and line: `3,077 keys · 7 failed · 5 calls` for a node that ran, `cached`, or
 * `skipped` with why (`upstream demand_forecast failed`, or the run stopped before it).
 */
export function runNodeOutcome(n: RunNodeSummary): { kind: StatusKind; label: string; detail: string | null } {
  if (n.status === 'cached') return { kind: 'skipped', label: 'cached', detail: null }
  if (n.status === 'skipped') {
    return { kind: 'skipped', label: 'skipped', detail: n.blocked_by ? `upstream ${shortName(n.blocked_by)} failed` : 'the run stopped before it' }
  }
  return { kind: n.failed ? 'failed' : 'success', label: runNodeLine(n), detail: null }
}

export function runLabel(status: string): string {
  const known = parseRunStatus(status)
  return known === 'pending' ? 'queued' : known ?? status
}

/** The runs list's poll: every 2s while a listed run is live (its status changes), else every 10s. */
export const RUNS_LIVE_POLL_MS = 2_000
export const RUNS_IDLE_POLL_MS = 10_000

export function runsPollInterval(runs: readonly { status: string }[] | undefined): number {
  return runs?.some((r) => runIsLive(r.status)) ? RUNS_LIVE_POLL_MS : RUNS_IDLE_POLL_MS
}

/** `pending` and `running` are live; every other status is terminal. */
export function runIsLive(status: string): boolean {
  const known = parseRunStatus(status)
  return known === 'pending' || known === 'running'
}

/** What started a run, for a run list; `null` (runs recorded before barca kept it) is a dash. */
export function triggeredByLabel(triggeredBy: TriggeredBy | null): string {
  if (triggeredBy === null) return '–'
  return match(triggeredBy)
    .with('schedule', () => 'schedule')
    .with('api', () => 'api')
    .with('client', () => 'client')
    .with('manual', () => 'manual')
    .exhaustive()
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function parseEnvelope(value: unknown): ErrorEnvelope | null {
  if (!isRecord(value) || typeof value.error !== 'string') return null
  const str = (v: unknown) => (typeof v === 'string' ? v : null)
  const kind = value.kind
  return {
    error: value.error,
    code: typeof value.code === 'number' ? value.code : 3,
    kind: kind === 'step_failed' || kind === 'usage' || kind === 'cancelled' ? kind : 'infra',
    remediation: str(value.remediation),
    node: str(value.node),
    traceback: str(value.traceback),
    artifact_dir: str(value.artifact_dir),
  }
}

/**
 * Narrow a run's `outcome` (an untyped object on the wire) to `RunOutcome`.
 * Null while the run has no outcome, or when the object is not one this UI
 * understands.
 */
export function parseOutcome(raw: Record<string, unknown> | null | undefined): RunOutcome | null {
  if (!raw) return null
  const status = raw.status
  if (status === 'success') {
    return {
      status,
      run_id: typeof raw.run_id === 'string' ? raw.run_id : '',
      ...('final_output' in raw ? { final_output: raw.final_output } : {}),
      ...(isRecord(raw.targets) ? { targets: raw.targets } : {}),
    }
  }
  if (status === 'failed' || status === 'cancelled' || status === 'error') {
    const envelope = parseEnvelope(raw.envelope)
    return envelope ? { status, envelope } : null
  }
  return null
}

/** The value a successful run returned: one target's `final_output`, or several `targets`. */
export function outcomeResult(outcome: RunOutcome | null): unknown {
  if (outcome === null) return undefined
  return match(outcome)
    .with({ status: 'success' }, (o) => o.final_output ?? o.targets)
    .with({ status: 'failed' }, { status: 'cancelled' }, { status: 'error' }, () => undefined)
    .exhaustive()
}

/** A run's start or finish in the reader's zone; `null` (not finished, or not recorded) is a dash. */
export function runTime(value: string | null, zone: TimeZone): string {
  if (!value) return '–'
  return formatAbsolute(value, zone)
}
